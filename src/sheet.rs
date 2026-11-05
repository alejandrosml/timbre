//! Style sheets as Mail applies them.
//!
//! A message's `<style>` elements are read with cssparser and their rules
//! matched with the selectors crate: the CSS tokenizer and selector engine
//! of Firefox, which scraper is built on. So comments, escapes, strings and
//! stray braces are read as a browser reads them, and a selector holds
//! exactly where it would: classes and ids (case-insensitive in quirks
//! mode), attributes, what is above or beside an element, `:not()`,
//! `:nth-child()`, `:has()`. Rules apply in cascade order: specificity,
//! then position, rules in `@layer` before the others, `!important` above
//! all (see [`crate::css::Computed`]).
//!
//! What Mail on a Mac does not apply is left out: media queries for narrow
//! screens, print or dark mode, `@supports` conditions WebKit does not meet,
//! rules whose selector list is invalid in WebKit (dropped whole, as there),
//! and states a message at rest is not in (`:hover`, `:focus`, `:visited`).
//!
//! A hostile style sheet cannot make reading a message take minutes: rules,
//! selector checks and steps through the document are counted, and past a
//! limit the message's text is withheld ([`Sheet::overflowed`]).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState, QualifiedRuleParser,
    RuleBodyItemParser, RuleBodyParser, StyleSheetParser, ToCss,
};
use ego_tree::NodeId;
use html5ever::Namespace;
use scraper::ElementRef;
use scraper::selector::{CssLocalName, CssString};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::matching::{
    self, ElementSelectorFlags, MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::parser::{self as sp, Component, ParseRelative, Selector, SelectorList, SelectorParseErrorKind};
use selectors::{Element, OpaqueElement};

use crate::animation::Keyframes;
use crate::css::{self, Computed, Decls};

/// Selector checks and document steps allowed per message.
const BUDGET: usize = 3_000_000;
/// Selectors (each member of a selector list) read per message; beyond, the
/// message's text is withheld.
const MAX_RULES: usize = 20_000;
/// Nesting of rules (`@media`, `@layer`, style rules inside style rules…)
/// followed; deeper, the message's text is withheld.
const MAX_NESTING: usize = 16;
/// Cascade layers and `@container` rules per message.
const MAX_LAYERS: usize = 10_000;
/// Parts of one selector, those of the selectors inside it included; a
/// heavier one is no real mail's.
const MAX_WEIGHT: usize = 10_000;

// ------------------------------------------------------------ selector glue

/// The selector flavour of this module: scraper's names and values, and the
/// pseudo-classes and pseudo-elements WebKit knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailImpl;

impl sp::SelectorImpl for MailImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssLocalName;
    type LocalName = CssLocalName;
    type NamespaceUrl = Namespace;
    type NamespacePrefix = CssLocalName;
    type BorrowedNamespaceUrl = Namespace;
    type BorrowedLocalName = CssLocalName;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoEl;
}

/// The pseudo-classes WebKit accepts, as far as a message at rest goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoClass {
    /// A state a message being read is never in: `:hover`, `:focus`,
    /// `:visited`, `:target`, media playing…
    Never,
    Link,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Open,
    Lang(Vec<String>),
    /// `:dir(rtl)` (true) or `:dir(ltr)`.
    Dir(bool),
    Valid,
    Invalid,
    Default,
    /// Audio and video: none plays in a message.
    Paused,
    Indeterminate,
    /// Elements are defined, except custom ones (no scripts define them).
    Defined,
    /// Audio and video with the `muted` attribute.
    Muted,
    /// `:heading` (h1 to h6), or headings of these levels.
    Heading(Vec<u32>),
    /// Fields with a range whose value is inside it (true) or outside.
    InRange(bool),
}

impl ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        write!(dest, ":{self:?}")
    }
}

impl sp::NonTSPseudoClass for PseudoClass {
    fn is_active_or_hover(&self) -> bool {
        false
    }

    fn is_user_action_state(&self) -> bool {
        false
    }
}

/// A pseudo-element WebKit accepts. `::first-line` and `::first-letter`
/// style the start of an element's own text (see [`css::FirstText`]); the
/// others style generated content and are not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoEl {
    FirstLine,
    FirstLetter,
    /// `::slotted(…)` in a shadow tree's style sheet: the children of its
    /// host that a slot shows, if they match the selectors given.
    Slotted(NodeId, SelectorList<MailImpl>),
    Other,
    /// A `-webkit-` pseudo-element WebKit does not know.
    UnknownWebkit,
}

impl ToCss for PseudoEl {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str("::pseudo")
    }
}

impl sp::PseudoElement for PseudoEl {}

/// Pseudo-classes of states that never hold while a message is read.
const NEVER: &[&str] = &[
    "active",
    "active-view-transition",
    "autofill",
    "buffering",
    "corner-present",
    "decrement",
    "double-button",
    "end",
    "focus",
    "focus-visible",
    "focus-within",
    "fullscreen",
    "future",
    "horizontal",
    "hover",
    "increment",
    "modal",
    "no-button",
    "past",
    "picture-in-picture",
    "playing",
    "popover-open",
    "seeking",
    "single-button",
    "stalled",
    "start",
    "target",
    "user-invalid",
    "user-valid",
    "vertical",
    "visited",
    "volume-locked",
    "window-inactive",
    "-apple-has-attachment",
    "-webkit-autofill",
    "-webkit-autofill-and-obscured",
    "-webkit-autofill-strong-password",
    "-webkit-autofill-strong-password-viewable",
    "-webkit-drag",
    "-webkit-full-screen",
];

/// Pseudo-elements WebKit accepts (any `::-webkit-…` too).
const PSEUDO_ELEMENTS: &[&str] = &[
    "after",
    "backdrop",
    "before",
    "checkmark",
    "cue",
    "details-content",
    "field-content",
    "file-selector-button",
    "first-letter",
    "first-line",
    "grammar-error",
    "marker",
    "picker-icon",
    "placeholder",
    "selection",
    "spelling-error",
    "target-text",
    "view-transition",
    "-apple-attachment-controls-container",
];

/// The `-webkit-` pseudo-elements WebKit knows. Others are taken in style
/// rules (they match nothing) but fail `@supports selector()`.
const WEBKIT_PSEUDO_ELEMENTS: &[&str] = &[
    "-webkit-caps-lock-indicator",
    "-webkit-color-swatch",
    "-webkit-color-swatch-wrapper",
    "-webkit-contacts-auto-fill-button",
    "-webkit-credentials-auto-fill-button",
    "-webkit-credit-card-auto-fill-button",
    "-webkit-date-and-time-value",
    "-webkit-datetime-edit",
    "-webkit-datetime-edit-day-field",
    "-webkit-datetime-edit-fields-wrapper",
    "-webkit-datetime-edit-hour-field",
    "-webkit-datetime-edit-meridiem-field",
    "-webkit-datetime-edit-millisecond-field",
    "-webkit-datetime-edit-minute-field",
    "-webkit-datetime-edit-month-field",
    "-webkit-datetime-edit-second-field",
    "-webkit-datetime-edit-text",
    "-webkit-datetime-edit-year-field",
    "-webkit-file-upload-button",
    "-webkit-generic-cue-root",
    "-webkit-inner-spin-button",
    "-webkit-input-placeholder",
    "-webkit-list-button",
    "-webkit-media-controls",
    "-webkit-media-text-track-container",
    "-webkit-media-text-track-display",
    "-webkit-media-text-track-display-backdrop",
    "-webkit-media-text-track-region",
    "-webkit-media-text-track-region-container",
    "-webkit-meter-bar",
    "-webkit-meter-even-less-good-value",
    "-webkit-meter-inner-element",
    "-webkit-meter-optimum-value",
    "-webkit-meter-suboptimum-value",
    "-webkit-progress-bar",
    "-webkit-progress-inner-element",
    "-webkit-progress-value",
    "-webkit-resizer",
    "-webkit-scrollbar",
    "-webkit-scrollbar-button",
    "-webkit-scrollbar-corner",
    "-webkit-scrollbar-thumb",
    "-webkit-scrollbar-track",
    "-webkit-scrollbar-track-piece",
    "-webkit-search-cancel-button",
    "-webkit-search-decoration",
    "-webkit-search-results-button",
    "-webkit-search-results-decoration",
    "-webkit-slider-container",
    "-webkit-slider-runnable-track",
    "-webkit-slider-thumb",
    "-webkit-strong-password-auto-fill-button",
    "-webkit-textfield-decoration-container",
    "-webkit-validation-bubble",
    "-webkit-validation-bubble-arrow",
    "-webkit-validation-bubble-arrow-clipper",
    "-webkit-validation-bubble-body",
    "-webkit-validation-bubble-heading",
    "-webkit-validation-bubble-icon",
    "-webkit-validation-bubble-message",
    "-webkit-validation-bubble-text-block",
];

/// Reads selectors the way WebKit accepts them; anything else is invalid.
#[derive(Clone, Copy, Default)]
struct SelParser<'a> {
    /// Inside a style rule, where `&` names the rule's selector.
    nesting: bool,
    /// In a shadow tree's style sheet: its host, for `:host`.
    host: Option<NodeId>,
    /// `@namespace` prefixes and the default namespace of the sheet.
    namespaces: Option<&'a Namespaces>,
    /// For `@supports selector()`: every argument must parse, and unknown
    /// `-webkit-` pseudo-elements fail.
    strict: bool,
}

/// The `@namespace` rules of a style sheet.
#[derive(Clone, Debug, Default)]
pub struct Namespaces {
    default: Option<Namespace>,
    prefixes: Vec<(String, Namespace)>,
}

impl<'i> sp::Parser<'i> for SelParser<'_> {
    type Impl = MailImpl;
    type Error = SelectorParseErrorKind;

    fn parse_is_and_where(&self) -> bool {
        true
    }

    fn parse_has(&self) -> bool {
        true
    }

    fn parse_nth_child_of(&self) -> bool {
        true
    }

    fn parse_parent_selector(&self) -> bool {
        self.nesting
    }

    /// `:host` and `:host(…)`, in a shadow tree's style sheet.
    fn parse_host(&self) -> bool {
        self.host.is_some()
    }

    fn parse_part(&self) -> bool {
        true
    }

    fn allow_forgiving_selectors(&self) -> bool {
        !self.strict
    }

    fn default_namespace(&self) -> Option<Namespace> {
        self.namespaces.and_then(|n| n.default.clone())
    }

    fn namespace_for_prefix(&self, prefix: &CssLocalName) -> Option<Namespace> {
        self.namespaces?.prefixes.iter().rev().find(|(p, _)| *p == *prefix.0).map(|(_, ns)| ns.clone())
    }

    fn is_is_alias(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("-webkit-any") || name.eq_ignore_ascii_case("matches")
    }

    fn parse_non_ts_pseudo_class(&self, name: CowRcStr<'i>) -> Result<PseudoClass, ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "link" | "any-link" | "-webkit-any-link" => PseudoClass::Link,
            "checked" => PseudoClass::Checked,
            "disabled" => PseudoClass::Disabled,
            "enabled" => PseudoClass::Enabled,
            "required" => PseudoClass::Required,
            "optional" => PseudoClass::Optional,
            "read-only" => PseudoClass::ReadOnly,
            "read-write" => PseudoClass::ReadWrite,
            "placeholder-shown" => PseudoClass::PlaceholderShown,
            "open" => PseudoClass::Open,
            "defined" => PseudoClass::Defined,
            "muted" => PseudoClass::Muted,
            "heading" => PseudoClass::Heading(vec![]),
            "in-range" => PseudoClass::InRange(true),
            "out-of-range" => PseudoClass::InRange(false),
            // Outside a shadow tree's style sheet, :host matches nothing.
            "host" => PseudoClass::Never,
            "valid" => PseudoClass::Valid,
            "invalid" => PseudoClass::Invalid,
            "default" => PseudoClass::Default,
            "paused" => PseudoClass::Paused,
            "indeterminate" => PseudoClass::Indeterminate,
            n if NEVER.contains(&n) => PseudoClass::Never,
            _ => return Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)),
        })
    }

    fn parse_non_ts_functional_pseudo_class(
        &self,
        name: CowRcStr<'i>,
        parser: &mut Parser<'i>,
        _after_part: bool,
    ) -> Result<PseudoClass, ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "lang" => {
                let langs = parser.parse_comma_separated(|p| Ok(p.expect_ident_or_string()?.to_ascii_lowercase()))?;
                Ok(PseudoClass::Lang(langs))
            }
            "dir" => {
                let dir = parser.expect_ident()?.to_ascii_lowercase();
                match dir.as_str() {
                    "ltr" => Ok(PseudoClass::Dir(false)),
                    "rtl" => Ok(PseudoClass::Dir(true)),
                    _ => Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)),
                }
            }
            "heading" => {
                let levels = parser.parse_comma_separated(|p| {
                    let n = p.expect_integer()?;
                    Ok(u32::try_from(n).unwrap_or(0))
                })?;
                Ok(PseudoClass::Heading(levels))
            }
            "state" | "active-view-transition-type" => {
                while parser.next().is_ok() {}
                Ok(PseudoClass::Never)
            }
            _ => Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)),
        }
    }

    fn parse_pseudo_element(&self, name: CowRcStr<'i>) -> Result<PseudoEl, ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        if lower == "first-line" {
            Ok(PseudoEl::FirstLine)
        } else if lower == "first-letter" {
            Ok(PseudoEl::FirstLetter)
        } else if PSEUDO_ELEMENTS.contains(&lower.as_str()) || WEBKIT_PSEUDO_ELEMENTS.contains(&lower.as_str()) {
            Ok(PseudoEl::Other)
        } else if lower.starts_with("-webkit-") && !self.strict {
            Ok(PseudoEl::UnknownWebkit)
        } else {
            Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement))
        }
    }

    fn parse_functional_pseudo_element(
        &self,
        name: CowRcStr<'i>,
        arguments: &mut Parser<'i>,
    ) -> Result<PseudoEl, ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        if let ("slotted", Some(host)) = (lower.as_str(), self.host) {
            let inner = SelParser { nesting: false, host: None, ..*self };
            let list = SelectorList::parse(&inner, arguments, ParseRelative::No)?;
            return Ok(PseudoEl::Slotted(host, list));
        }
        if matches!(
            lower.as_str(),
            "highlight"
                | "cue"
                | "slotted"
                | "picker"
                | "view-transition-group"
                | "view-transition-image-pair"
                | "view-transition-new"
                | "view-transition-old"
        ) {
            while arguments.next().is_ok() {}
            Ok(PseudoEl::Other)
        } else {
            Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement))
        }
    }
}

/// What is left of the budget, and whether it ran out.
#[derive(Debug)]
struct Meter {
    left: Cell<usize>,
    over: Cell<bool>,
}

impl Meter {
    fn spend(&self, n: usize) -> bool {
        let left = self.left.get();
        if n <= left {
            self.left.set(left - n);
            true
        } else {
            self.left.set(0);
            self.over.set(true);
            false
        }
    }
}

/// An element as the selector engine walks the document: each step to a
/// parent, a sibling or a child is paid for.
#[derive(Clone, Copy, Debug)]
pub struct Node<'a> {
    e: ElementRef<'a>,
    meter: &'a Meter,
    /// The `<template>`s that give their parent a shadow tree: not part of
    /// the document as selectors see it.
    templates: &'a HashSet<NodeId>,
}

impl<'a> Node<'a> {
    fn wrap(&self, e: ElementRef<'a>) -> Node<'a> {
        Node { e, meter: self.meter, templates: self.templates }
    }

    /// The first element among `nodes`, paying one step per node passed.
    fn first_element(&self, nodes: impl Iterator<Item = ego_tree::NodeRef<'a, scraper::Node>>) -> Option<Node<'a>> {
        for n in nodes {
            if !self.meter.spend(1) {
                return None;
            }
            if let Some(e) = ElementRef::wrap(n).filter(|e| !self.templates.contains(&e.id())) {
                return Some(self.wrap(e));
            }
        }
        None
    }

    fn name(&self) -> &str {
        self.e.value().name()
    }

    fn attr(&self, name: &str) -> Option<&'a str> {
        self.e.value().attr(name)
    }

    /// Could the user type into it?
    fn editable(&self) -> bool {
        let text_input = || {
            self.attr("type").is_none_or(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "" | "text"
                        | "search"
                        | "url"
                        | "tel"
                        | "email"
                        | "password"
                        | "number"
                        | "date"
                        | "month"
                        | "week"
                        | "time"
                        | "datetime-local"
                )
            })
        };
        let form = match self.name() {
            "textarea" => true,
            "input" => text_input(),
            _ => false,
        };
        (form && self.attr("readonly").is_none() && self.attr("disabled").is_none())
            || self.attr("contenteditable").is_some_and(|v| !v.eq_ignore_ascii_case("false"))
    }

    /// Is a required field left empty?
    fn missing_value(&self) -> bool {
        if self.attr("required").is_none() {
            return false;
        }
        match self.name() {
            "input" => self.attr("value").is_none_or(str::is_empty),
            "textarea" => !self.e.children().any(|c| c.value().as_text().is_some_and(|t| !t.trim().is_empty())),
            _ => false,
        }
    }

    /// Written right to left, from the nearest `dir`.
    fn rtl(&self) -> bool {
        let mut at = Some(self.e);
        while let Some(e) = at {
            if !self.meter.spend(1) {
                return false;
            }
            match e.value().attr("dir").map(str::to_ascii_lowercase).as_deref() {
                Some("rtl") => return true,
                Some("ltr") => return false,
                _ => {}
            }
            at = e.parent().and_then(ElementRef::wrap);
        }
        false
    }

    /// An option a single-choice list shows without a `selected` one: the
    /// first.
    fn first_shown_option(&self) -> bool {
        let mut select = self.e.parent().and_then(ElementRef::wrap);
        if select.is_some_and(|p| p.value().name() == "optgroup") {
            select = select.and_then(|p| p.parent()).and_then(ElementRef::wrap);
        }
        let Some(select) = select.filter(|s| s.value().name() == "select" && s.value().attr("multiple").is_none())
        else {
            return false;
        };
        let mut first = None;
        for (k, o) in
            select.descendants().filter_map(ElementRef::wrap).filter(|o| o.value().name() == "option").enumerate()
        {
            if !self.meter.spend(1) || k > 1000 {
                return false;
            }
            if o.value().attr("selected").is_some() {
                return false;
            }
            first.get_or_insert(o);
        }
        first.is_some_and(|o| o.id() == self.e.id())
    }

    /// For a field with a range (`min`, `max`, or a range slider): is its
    /// value inside it?
    fn in_range(&self) -> Option<bool> {
        if self.name() != "input" {
            return None;
        }
        let kind = self.attr("type").map(str::to_ascii_lowercase).unwrap_or_default();
        if kind == "range" {
            return Some(true);
        }
        if !matches!(kind.as_str(), "number" | "date" | "month" | "week" | "time" | "datetime-local") {
            return None;
        }
        let num = |a: &str| self.attr(a).and_then(|v| v.trim().parse::<f64>().ok());
        let (min, max) = (num("min"), num("max"));
        if min.is_none() && max.is_none() {
            return None;
        }
        let Some(v) = num("value") else { return Some(true) };
        Some(min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m))
    }

    /// The language the element is written in, from the nearest `lang`.
    fn lang(&self) -> Option<String> {
        let mut at = Some(self.e);
        while let Some(e) = at {
            if !self.meter.spend(1) {
                return None;
            }
            if let Some(l) = e.value().attr("lang") {
                return Some(l.to_ascii_lowercase());
            }
            at = e.parent().and_then(ElementRef::wrap);
        }
        None
    }
}

impl Element for Node<'_> {
    type Impl = MailImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.e.value())
    }

    fn parent_element(&self) -> Option<Self> {
        if !self.meter.spend(1) {
            return None;
        }
        let parent = self.e.parent()?;
        if let Some(e) = ElementRef::wrap(parent) {
            return Some(self.wrap(e));
        }
        // What a <template> holds sits in a fragment: above the top elements
        // of a shadow tree is its host.
        if parent.value().is_fragment() {
            let template =
                parent.parent().filter(|t| t.value().as_element().is_some_and(|t| t.name() == "template"))?;
            return template.parent().and_then(ElementRef::wrap).map(|e| self.wrap(e));
        }
        None
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    /// The host of the shadow tree the element is in.
    fn containing_shadow_host(&self) -> Option<Self> {
        let mut at = self.e.parent();
        while let Some(n) = at {
            if !self.meter.spend(1) {
                return None;
            }
            if let Some(t) = n.parent().filter(|t| n.value().is_fragment() && self.templates.contains(&t.id())) {
                return t.parent().and_then(ElementRef::wrap).map(|e| self.wrap(e));
            }
            at = n.parent();
        }
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.first_element(self.e.prev_siblings())
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.first_element(self.e.next_siblings())
    }

    fn first_element_child(&self) -> Option<Self> {
        self.first_element(self.e.children())
    }

    fn is_html_element_in_html_document(&self) -> bool {
        &*self.e.value().name.ns == "http://www.w3.org/1999/xhtml"
    }

    fn has_local_name(&self, name: &CssLocalName) -> bool {
        self.e.value().name.local == name.0
    }

    fn has_namespace(&self, namespace: &Namespace) -> bool {
        &self.e.value().name.ns == namespace
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.e.value().name == other.e.value().name
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &CssLocalName,
        operation: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        // A substring test costs the attribute's length times the text's.
        let wanted = match operation {
            AttrSelectorOperation::WithValue { value, .. } => value.0.len(),
            AttrSelectorOperation::Exists => 0,
        };
        self.e.value().attrs.iter().any(|(key, value)| {
            !matches!(*ns, NamespaceConstraint::Specific(url) if *url != key.ns)
                && local_name.0 == key.local
                && self.meter.spend(value.len() * wanted.max(1) / 256 + 1)
                && operation.eval_str(value)
        })
    }

    fn match_non_ts_pseudo_class(&self, pc: &PseudoClass, _context: &mut MatchingContext<'_, Self::Impl>) -> bool {
        let name = self.name();
        let control = matches!(name, "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset");
        let field = matches!(name, "input" | "select" | "textarea");
        match pc {
            PseudoClass::Never => false,
            PseudoClass::Link => self.is_link(),
            PseudoClass::Checked => {
                (name == "input"
                    && self.attr("checked").is_some()
                    && self
                        .attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")))
                    || (name == "option" && (self.attr("selected").is_some() || self.first_shown_option()))
            }
            PseudoClass::Disabled => control && self.attr("disabled").is_some(),
            PseudoClass::Enabled => control && self.attr("disabled").is_none(),
            PseudoClass::Required => field && self.attr("required").is_some(),
            PseudoClass::Optional => field && self.attr("required").is_none(),
            PseudoClass::ReadWrite => self.editable(),
            PseudoClass::ReadOnly => !self.editable(),
            PseudoClass::PlaceholderShown => {
                matches!(name, "input" | "textarea")
                    && self.attr("placeholder").is_some()
                    && self.attr("value").is_none_or(str::is_empty)
            }
            PseudoClass::Open => matches!(name, "details" | "dialog") && self.attr("open").is_some(),
            PseudoClass::Lang(wanted) => self
                .lang()
                .is_some_and(|l| wanted.iter().any(|w| l == *w || l.starts_with(&format!("{w}-")) || w == "*")),
            PseudoClass::Dir(rtl) => self.rtl() == *rtl,
            PseudoClass::Valid => (field || matches!(name, "form" | "fieldset")) && !self.missing_value(),
            PseudoClass::Invalid => field && self.missing_value(),
            PseudoClass::Default => {
                (name == "input"
                    && self.attr("checked").is_some()
                    && self
                        .attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")))
                    || (name == "option" && self.attr("selected").is_some())
                    || (name == "button" && self.attr("type").is_none_or(|t| t.eq_ignore_ascii_case("submit")))
                    || (name == "input" && self.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("submit")))
            }
            PseudoClass::Paused => matches!(name, "audio" | "video"),
            PseudoClass::Indeterminate => name == "progress" && self.attr("value").is_none(),
            PseudoClass::Defined => !name.contains('-'),
            PseudoClass::Muted => matches!(name, "audio" | "video") && self.attr("muted").is_some(),
            PseudoClass::Heading(levels) => {
                let level = match name {
                    "h1" => 1,
                    "h2" => 2,
                    "h3" => 3,
                    "h4" => 4,
                    "h5" => 5,
                    "h6" => 6,
                    _ => return false,
                };
                levels.is_empty() || levels.contains(&level)
            }
            PseudoClass::InRange(inside) => self.in_range() == Some(*inside),
        }
    }

    fn match_pseudo_element(&self, pe: &PseudoEl, _context: &mut MatchingContext<'_, Self::Impl>) -> bool {
        matches!(pe, PseudoEl::FirstLine | PseudoEl::FirstLetter)
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        matches!(self.name(), "a" | "area" | "link") && self.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        self.name() == "slot"
    }

    fn has_id(&self, id: &CssLocalName, case_sensitivity: CaseSensitivity) -> bool {
        self.e.value().id().is_some_and(|v| case_sensitivity.eq(id.0.as_bytes(), v.as_bytes()))
    }

    fn has_class(&self, name: &CssLocalName, case_sensitivity: CaseSensitivity) -> bool {
        // Paid for by the class looked at: an element may have thousands.
        for (k, c) in self.e.value().classes().enumerate() {
            if k % 16 == 15 && !self.meter.spend(1) {
                return false;
            }
            if case_sensitivity.eq(c.as_bytes(), name.0.as_bytes()) {
                return true;
            }
        }
        false
    }

    fn has_custom_state(&self, _name: &CssLocalName) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssLocalName) -> Option<CssLocalName> {
        None
    }

    fn is_part(&self, name: &CssLocalName) -> bool {
        self.attr("part").is_some_and(|v| v.split_ascii_whitespace().any(|p| p == &*name.0))
    }

    fn is_empty(&self) -> bool {
        !self.e.children().any(|c| {
            (c.value().is_element() && !self.templates.contains(&c.id()))
                || c.value().as_text().is_some_and(|t| !t.is_empty())
        })
    }

    fn is_root(&self) -> bool {
        self.e.parent().is_some_and(|p| p.value().is_document())
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

// ------------------------------------------------------------- the sheet

/// One selector of a rule, with what it sets and where it stands in the
/// cascade.
struct Rule {
    selector: Selector<MailImpl>,
    decls: usize,
    /// The cascade layer (see [`Sheet::ranks`]); `None` outside layers.
    layer: Option<u32>,
    specificity: u32,
    position: usize,
    /// Aimed at `::first-line` or `::first-letter`.
    first: Option<First>,
    /// Inside `@container`: its query, in [`Sheet::queries`].
    container: Option<u32>,
    /// The tree the rule styles: the document (`None`) or a shadow tree,
    /// by its `<template>`.
    tree: Option<NodeId>,
    /// From a shadow tree's sheet, styling its host or the host's children
    /// (`:host`, `::slotted()`): the host's own rules win over it, unless
    /// it is `!important`.
    inner: bool,
    /// `::slotted()`: only for children of this host.
    child_of: Option<NodeId>,
    /// From a shadow tree's sheet: the tree's host, which `:host` names.
    host: Option<NodeId>,
    /// Inside `@scope`: in [`Sheet::scopes`].
    scope: Option<u32>,
    /// `::part()`: styles elements of the shadow trees of its tree.
    part: bool,
    /// What matching it may cost (see [`weight`]).
    weight: usize,
}

impl Rule {
    /// The tree context: a shadow tree's rules for its host or the host's
    /// children (0), the element's own tree (1), `::part()` rules from
    /// outside (2).
    fn context(&self) -> u8 {
        if self.inner {
            0
        } else if self.part {
            2
        } else {
            1
        }
    }

    /// Order of normal declarations, the winner last: the outer tree
    /// context; later layers, rules outside layers after all; then
    /// specificity, then the nearest `@scope` root (`near`), then position.
    fn normal_rank(&self, ranks: &[u32], near: u32) -> (u8, u32, u32, u32, usize) {
        (self.context(), self.layer.map_or(u32::MAX, |l| ranks[l as usize]), self.specificity, near, self.position)
    }

    /// Order of `!important` ones: the contexts and the layers in reverse,
    /// rules outside layers before all.
    fn important_rank(&self, ranks: &[u32], near: u32) -> (u8, u32, u32, u32, usize) {
        let layer = self.layer.map_or(0, |l| u32::MAX - 1 - ranks[l as usize]);
        (2 - self.context(), layer, self.specificity, near, self.position)
    }
}

/// An `@keyframes` rule, and where it stands: for a name, the rules of the
/// element's tree win, then the last of the winning layer.
struct KeyframesRule {
    frames: Keyframes,
    layer: Option<u32>,
    tree: Option<NodeId>,
    position: usize,
}

/// The condition of an `@container` rule.
struct Query {
    name: Option<String>,
    /// `None`: only the container's name is asked for.
    condition: Option<String>,
    /// The sizes it asks about, across and down.
    width: bool,
    height: bool,
    /// The `@container` rule this one is inside.
    up: Option<u32>,
}

/// An element `@container` rules may ask about (see [`Sheet::compute_in`]).
#[derive(Debug, Clone)]
pub struct Container {
    names: Vec<String>,
    /// `container-type: size` (true) or `inline-size` (false); `None` when
    /// only named, for style queries.
    size: Option<bool>,
    width: (f32, f32),
    height: (f32, f32),
    /// Containment applies to its box, so size queries can measure it.
    sized: bool,
    /// Its font size: queries in `em` count from it.
    font: f32,
    vars: Option<Rc<css::Vars>>,
}

impl Container {
    /// The container an element with this style is, if any: `width` is the
    /// range its width is in (see [`css::Look`]), `vars` its custom
    /// properties; `sized`: containment applies to its box (see
    /// [`css::sized_box`]), else size queries cannot measure it.
    pub fn of(
        style: &Computed,
        width: (f32, f32),
        vars: &Option<Rc<css::Vars>>,
        sized: bool,
        font: f32,
    ) -> Option<Container> {
        let kind = |t: &str| match t.trim() {
            "size" => Some(true),
            "inline-size" => Some(false),
            _ => None,
        };
        let names_of = |v: &str| v.split_whitespace().filter(|n| *n != "none").map(String::from).collect::<Vec<_>>();
        let (mut names, mut size) = (vec![], None);
        if let Some(v) = style.get("container") {
            let (n, t) = v.split_once('/').unwrap_or((v, ""));
            names = names_of(n);
            size = kind(t);
        }
        if let Some(v) = style.get("container-name") {
            names = names_of(v);
        }
        if let Some(v) = style.get("container-type") {
            size = kind(v);
        }
        if names.is_empty() && size.is_none() {
            return None;
        }
        // Contained in size, a box has the height it is given, or none.
        let px = |p: &str| style.get(p).and_then(css::px).filter(|n| n.is_finite()).unwrap_or(0.0).max(0.0);
        let h = px("height").max(px("min-height"));
        Some(Container { names, size, width, height: (h, h), sized, font, vars: vars.clone() })
    }

    /// Its size for container units (`cqw`…): width, height (none when
    /// only its width is contained), at their least; `None`: not sized.
    pub fn units(&self) -> Option<(f64, Option<f64>)> {
        let size = self.size.filter(|_| self.sized)?;
        Some((self.width.0.into(), size.then_some(self.height.0.into())))
    }
}

/// Where an element stands, for the rules that depend on it.
#[derive(Default)]
pub struct Around<'a> {
    /// The containers around the element, innermost last.
    pub containers: &'a [Container],
    /// The custom properties the element inherits.
    pub vars: Option<Rc<css::Vars>>,
    /// The shadow tree the element is in, by its `<template>`.
    pub tree: Option<NodeId>,
}

/// Where a `<style>` element is, for the rules that depend on it.
#[derive(Default, Clone, Copy)]
pub struct Origin {
    /// The element it is in.
    pub owner: Option<NodeId>,
    /// The shadow tree it is in, by its `<template>`; its host, and the
    /// tree the host is in.
    pub tree: Option<NodeId>,
    pub host: Option<NodeId>,
    pub host_tree: Option<NodeId>,
}

/// The style rules of a message that apply in Mail.
pub struct Sheet {
    rules: Vec<Rule>,
    decls: Vec<Decls>,
    /// Each selector is filed once, by what the element it styles must
    /// have: its id, else a class, else its tag (all lower-cased).
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    quirks: bool,
    meter: Meter,
    caches: RefCell<SelectorCaches>,
    /// Hosts of style sheets the message loads from the web.
    remote: Vec<String>,
    /// Style attributes already read: mail repeats the same ones on
    /// hundreds of cells.
    inline: RefCell<HashMap<String, Rc<Decls>>>,
    /// Cascade layers by full name (`a.b`), and the place of each among
    /// its siblings, from the outermost.
    layer_ids: HashMap<String, u32>,
    layer_paths: Vec<Vec<u32>>,
    /// Layers named so far inside each layer ("" for the top).
    layer_children: HashMap<String, u32>,
    layer_ranks: OnceCell<Vec<u32>>,
    /// Rules read so far, for their position in the cascade.
    positions: usize,
    keyframes: HashMap<String, Vec<KeyframesRule>>,
    queries: Vec<Query>,
    /// The window the rules are for (one of [`css::WINDOWS`]).
    window: css::Window,
    /// Some rule applies in one of [`css::WINDOWS`] and not the other.
    window_dependent: bool,
    /// The widths size queries compare containers with, in CSS pixels for
    /// a 16-pixel font, and whether they follow the font (`em`).
    breakpoints: Vec<(f32, bool)>,
    /// What all readings at those widths may spend together.
    probe_budget: Cell<usize>,
    /// The width containers of an open width are taken to have in this
    /// reading (see [`Sheet::with_probe`]).
    probe: Cell<Option<f32>>,
    /// A size query asked about a container whose width is left open: the
    /// font sizes of those containers.
    uncertain: RefCell<Vec<f32>>,
    /// The `<template>`s that give their parent a shadow tree.
    templates: HashSet<NodeId>,
    /// `@scope` rules.
    scopes: Vec<Scope>,
    /// A `:where(:scope)` list: the parent of the rules of `@scope`.
    scope_parent: Option<SelectorList<MailImpl>>,
}

/// Where an `@scope` rule's rules apply: inside its roots, out of its
/// limits.
struct Scope {
    root: ScopeRoot,
    limit: Option<SelectorList<MailImpl>>,
}

enum ScopeRoot {
    Selectors(SelectorList<MailImpl>),
    /// `@scope` without a root: the element the `<style>` is in.
    Element(NodeId),
}

/// Hosts of web fonts: their style sheets only declare fonts.
const FONT_HOSTS: &[&str] = &["fonts.googleapis.com", "use.typekit.net", "fonts.bunny.net", "use.fontawesome.com"];

/// What `::first-line` and `::first-letter` may change, of what is read
/// here (WebKit's allowlists: not `-webkit-text-fill-color` nor
/// `-webkit-text-stroke`).
const FIRST_TEXT: &[&str] = &[
    "color",
    "font",
    "font-size",
    "background",
    "background-color",
    "background-image",
    "background-clip",
    "text-shadow",
    "opacity",
    "visibility",
];

/// The start of an element's text a rule styles, rather than all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum First {
    /// `::first-line`
    Line,
    /// `::first-letter`
    Letter,
}

impl Sheet {
    /// `quirks`: the message has no modern doctype, so WebKit draws it in
    /// quirks mode (class names match whatever their case). `window`: where
    /// Mail draws the message (one of [`css::WINDOWS`]).
    pub fn new(quirks: bool, window: css::Window) -> Sheet {
        Sheet {
            rules: vec![],
            decls: vec![],
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_tag: HashMap::new(),
            universal: vec![],
            quirks,
            meter: Meter { left: Cell::new(BUDGET), over: Cell::new(false) },
            caches: RefCell::new(SelectorCaches::default()),
            remote: vec![],
            inline: RefCell::new(HashMap::new()),
            layer_ids: HashMap::new(),
            layer_paths: vec![],
            layer_children: HashMap::new(),
            layer_ranks: OnceCell::new(),
            positions: 0,
            keyframes: HashMap::new(),
            queries: vec![],
            window,
            window_dependent: false,
            breakpoints: vec![],
            probe_budget: Cell::new(BUDGET / 8),
            probe: Cell::new(None),
            uncertain: RefCell::new(vec![]),
            templates: HashSet::new(),
            scopes: vec![],
            scope_parent: None,
        }
    }

    /// The `<template>`s that give their parent a shadow tree.
    pub fn set_shadow_templates(&mut self, templates: HashSet<NodeId>) {
        self.templates = templates;
    }

    /// The window the rules are for.
    pub fn window(&self) -> &css::Window {
        &self.window
    }

    /// Does a media query hold in the window the rules are for? Notes
    /// whether it would in the other window.
    pub fn media(&mut self, query: &str) -> bool {
        let holds = css::media_applies(query, &self.window);
        if css::WINDOWS.iter().any(|w| css::media_applies(query, w) != holds) {
            self.window_dependent = true;
        }
        holds
    }

    /// Do some rules apply in one window and not the other?
    pub fn window_dependent(&self) -> bool {
        self.window_dependent
    }

    /// Count work done for the rules' sake elsewhere (`var()`…).
    pub fn charge(&self, n: usize) -> bool {
        self.meter.spend(n)
    }

    /// The declarations of a style attribute, read once per message.
    fn inline(&self, style: &str) -> Rc<Decls> {
        if let Some(d) = self.inline.borrow().get(style) {
            return d.clone();
        }
        let d = Rc::new(Decls::parse(style, self.quirks));
        if d.hostile() {
            self.meter.over.set(true);
        }
        let mut cache = self.inline.borrow_mut();
        if cache.len() < 4096 {
            cache.insert(style.to_string(), d.clone());
        }
        d
    }

    /// Too much to check: what hides text may have been missed, so the
    /// message's text is withheld rather than shown.
    pub fn overflowed(&self) -> bool {
        self.meter.over.get()
    }

    /// Run `f` with containers whose width is left open taken as `probe`
    /// pixels wide (as near as their range allows), on the budget all such
    /// readings share.
    pub fn with_probe<T>(&self, probe: f32, f: impl FnOnce() -> T) -> T {
        let left = self.meter.left.get();
        self.probe.set(Some(probe));
        self.meter.left.set(self.probe_budget.get());
        let out = f();
        self.probe_budget.set(self.meter.left.get());
        self.probe.set(None);
        self.meter.left.set(left);
        out
    }

    /// Start a new reading of the page: the budget of checks full again,
    /// unless it already ran out.
    pub fn new_reading(&self) {
        if !self.meter.over.get() {
            self.meter.left.set(BUDGET);
        }
    }

    /// Did a size query ask about a container whose width is left open?
    pub fn uncertain(&self) -> bool {
        !self.uncertain.borrow().is_empty()
    }

    /// The widths to read such a page at too: at each width size queries
    /// compare with, between each two, and past them, so a rule that holds
    /// for a band of widths only is seen. `None`: more of them than any real
    /// mail has.
    pub fn probes(&self) -> Option<Vec<f32>> {
        let fonts = self.uncertain.borrow();
        let mut widths: Vec<f32> = self
            .breakpoints
            .iter()
            .flat_map(|&(px, em)| {
                let fonts: Vec<f32> = if em { fonts.iter().map(|f| px * f / 16.0).collect() } else { vec![px] };
                fonts
            })
            .collect();
        widths.sort_by(f32::total_cmp);
        widths.dedup();
        if widths.len() > 10 {
            return None;
        }
        let mut out = vec![];
        for (i, w) in widths.iter().enumerate() {
            out.push(*w);
            if let Some(next) = widths.get(i + 1) {
                out.push((w + next) / 2.0);
            }
        }
        if let (Some(first), Some(last)) = (widths.first(), widths.last()) {
            out.extend([first - 1.0, last + 1.0]);
        }
        out.retain(|w| *w >= 0.0);
        Some(out)
    }

    /// The host of a style sheet the message loads from the web, other than
    /// a web font's: what it hides could not be checked.
    pub fn remote(&self) -> Option<&str> {
        self.remote.iter().map(String::as_str).find(|h| !FONT_HOSTS.contains(h))
    }

    /// Note a `<link rel="stylesheet">` or `@import`.
    pub fn note_remote(&mut self, url: &str) {
        let u = url.trim().trim_matches(|c| c == '"' || c == '\'');
        // A relative address has nothing to load from in a message.
        let lower = u.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")) {
            return;
        }
        let rest = u.split_once("//").map_or(u, |(_, r)| r);
        let host = rest.split(['/', '?', '#', ':', ')', '"', '\'']).next().unwrap_or("").to_ascii_lowercase();
        let host = host.rsplit('@').next().unwrap_or("").to_string();
        if !host.is_empty() && self.remote.len() < 8 {
            self.remote.push(host);
        }
    }

    /// Read a style sheet of the document.
    #[cfg(test)]
    pub fn add_css(&mut self, css: &str) {
        self.add_css_in(css, Origin::default());
    }

    /// Read one `<style>` element's text, the element being where `origin`
    /// says.
    pub fn add_css_in(&mut self, css: &str, origin: Origin) {
        self.layer_ranks.take();
        let ctx = Context {
            owner: origin.owner,
            tree: origin.tree,
            host: origin.host,
            host_tree: origin.host_tree,
            ..Context::default()
        };
        let mut input = Parser::new(css);
        let first = self.position();
        let mut reader = Reader::new(self, ctx, first);
        for _ in StyleSheetParser::new(&mut input, &mut reader) {}
    }

    /// The layer of this full name (`a.b` is `b` inside `a`), named for the
    /// first time or again.
    fn layer(&mut self, full: &str) -> u32 {
        let (mut name, mut id) = (String::new(), 0);
        let mut parent: Option<u32> = None;
        for part in full.split('.') {
            let parent_name = name.clone();
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(part);
            id = match self.layer_ids.get(&name) {
                Some(&id) => id,
                None => {
                    let mut path = parent.map_or(vec![], |p| self.layer_paths[p as usize].clone());
                    let n = self.layer_children.entry(parent_name).or_insert(0);
                    path.push(*n);
                    *n += 1;
                    let id = self.layer_paths.len() as u32;
                    self.layer_paths.push(path);
                    self.layer_ids.insert(name.clone(), id);
                    id
                }
            };
            parent = Some(id);
        }
        if self.layer_paths.len() > MAX_LAYERS {
            self.meter.over.set(true);
        }
        id
    }

    /// The order of the layers, the winner last: as first named, those
    /// inside a layer before the layer's own rules.
    fn ranks(&self) -> &[u32] {
        self.layer_ranks.get_or_init(|| {
            let mut ids: Vec<usize> = (0..self.layer_paths.len()).collect();
            ids.sort_by_cached_key(|&id| {
                let mut key = self.layer_paths[id].clone();
                key.push(u32::MAX);
                key
            });
            let mut ranks = vec![0; ids.len()];
            for (rank, id) in ids.into_iter().enumerate() {
                ranks[id] = rank as u32;
            }
            ranks
        })
    }

    /// A place in the cascade for a rule about to be read.
    fn position(&mut self) -> usize {
        self.positions += 1;
        self.positions
    }

    fn add_rule(&mut self, list: &SelectorList<MailImpl>, decls: Decls, ctx: &Context, position: usize) {
        if decls.hostile() {
            self.meter.over.set(true);
        }
        if decls.is_empty() {
            return;
        }
        let at_decls = self.decls.len();
        self.decls.push(decls);
        for selector in list.slice() {
            // ::slotted(X): X's rule, for the children of the host.
            let (selectors, child_of) = match selector.pseudo_element() {
                Some(PseudoEl::Slotted(host, x)) => (x.slice().to_vec(), Some(*host)),
                _ => (vec![selector.clone()], None),
            };
            for s in selectors {
                if self.rules.len() >= MAX_RULES {
                    self.meter.over.set(true);
                    return;
                }
                let mut left = MAX_WEIGHT;
                weight(&s, &mut left, 0);
                let cost = MAX_WEIGHT - left;
                if left == 0 || !self.meter.spend(cost) {
                    self.meter.over.set(true);
                    return;
                }
                let host_rule = child_of.is_some() || s.iter().any(|c| matches!(c, Component::Host(_)));
                let part = s.iter().any(|c| matches!(c, Component::Part(_)));
                let at = self.rules.len();
                match Key::of(&s) {
                    Key::Id(id) => self.by_id.entry(id).or_default().push(at),
                    Key::Class(c) => self.by_class.entry(c).or_default().push(at),
                    Key::Tag(t) => self.by_tag.entry(t).or_default().push(at),
                    Key::Any => self.universal.push(at),
                }
                self.rules.push(Rule {
                    decls: at_decls,
                    layer: ctx.layer,
                    specificity: s.specificity(),
                    position,
                    first: match s.pseudo_element() {
                        Some(PseudoEl::FirstLine) => Some(First::Line),
                        Some(PseudoEl::FirstLetter) => Some(First::Letter),
                        _ => None,
                    },
                    container: ctx.container,
                    tree: if host_rule { ctx.host_tree } else { ctx.tree },
                    inner: host_rule,
                    child_of,
                    host: ctx.host,
                    scope: ctx.scope,
                    part,
                    weight: cost.max(1),
                    selector: s,
                });
            }
        }
    }

    /// The declarations that apply to element `e` of the document with its
    /// style attribute.
    #[cfg(test)]
    pub fn compute(&self, e: ElementRef<'_>, style: Option<&str>) -> Computed {
        self.compute_in(e, style, &Around::default())
    }

    /// The declarations that apply to element `e` with its style attribute,
    /// where `around` says it is.
    pub fn compute_in(&self, e: ElementRef<'_>, style: Option<&str>, around: &Around<'_>) -> Computed {
        let mut out = Computed::default();
        let v = e.value();
        let mut lists: Vec<&Vec<usize>> = vec![];
        if !self.rules.is_empty() {
            if let Some(l) = v.id().and_then(|id| self.by_id.get(&id.to_ascii_lowercase())) {
                lists.push(l);
            }
            if !self.by_class.is_empty() {
                let mut classes: Vec<String> = v.classes().map(|c| c.to_ascii_lowercase()).collect();
                classes.sort_unstable();
                classes.dedup();
                lists.extend(classes.iter().filter_map(|c| self.by_class.get(c)));
            }
            lists.extend(self.by_tag.get(v.name()));
            if !self.universal.is_empty() {
                lists.push(&self.universal);
            }
        }
        // Rules that apply, with how near their `@scope` root is (0 when
        // unscoped: farthest).
        let mut hits: Vec<(usize, u32)> = vec![];
        let mut first_text: Vec<(usize, u32)> = vec![];
        if !lists.is_empty() {
            let node = Node { e, meter: &self.meter, templates: &self.templates };
            let parent = e.parent().map(|p| p.id());
            let mut queries: HashMap<u32, bool> = HashMap::new();
            let mut caches = self.caches.borrow_mut();
            let quirks = if self.quirks { QuirksMode::Quirks } else { QuirksMode::NoQuirks };
            'lists: for list in lists {
                for &at in list {
                    let rule = &self.rules[at];
                    if (rule.tree != around.tree && !rule.part)
                        || rule.child_of.is_some_and(|host| parent != Some(host))
                    {
                        continue;
                    }
                    if !self.meter.spend(rule.weight) {
                        break 'lists;
                    }
                    let mode = if rule.first.is_some() {
                        MatchingMode::ForStatelessPseudoElement
                    } else {
                        MatchingMode::Normal
                    };
                    let mut context = MatchingContext::new(
                        mode,
                        None,
                        &mut caches,
                        quirks,
                        NeedsSelectorFlags::No,
                        MatchingForInvalidation::No,
                    );
                    // The host :host names, for a shadow tree's rules.
                    context.current_host = rule
                        .host
                        .and_then(|h| e.tree().get(h))
                        .and_then(|h| h.value().as_element().map(OpaqueElement::new));
                    let near = match rule.scope {
                        None => {
                            if !matching::matches_selector(&rule.selector, 0, None, &node, &mut context) {
                                continue;
                            }
                            0
                        }
                        // The nearest root of the scope the selector holds from.
                        Some(scope) => {
                            let found = self.scope_roots(scope, &node).into_iter().find(|(root, _)| {
                                context.scope_element = Some(*root);
                                matching::matches_selector(&rule.selector, 0, None, &node, &mut context)
                            });
                            match found {
                                Some((_, distance)) => u32::MAX - distance,
                                None => continue,
                            }
                        }
                    };
                    if let Some(q) = rule.container
                        && !self.container_holds(q, around, &mut queries)
                    {
                        continue;
                    }
                    if rule.first.is_some() { first_text.push((at, near)) } else { hits.push((at, near)) }
                }
            }
        }
        let ranks = self.ranks();
        let own = style.map(|s| self.inline(s));
        let decls = |at: usize| &self.decls[self.rules[at].decls];
        hits.sort_unstable_by_key(|&(at, near)| self.rules[at].normal_rank(ranks, near));
        for &(at, _) in &hits {
            out.apply_only(decls(at), Some(false));
        }
        if let Some(own) = &own {
            out.apply_only(own, Some(false));
        }
        let mut important = hits.clone();
        important.sort_unstable_by_key(|&(at, near)| self.rules[at].important_rank(ranks, near));
        let apply_important = |out: &mut Computed| {
            for &(at, _) in &important {
                out.apply_only(decls(at), Some(true));
            }
            if let Some(own) = &own {
                out.apply_only(own, Some(true));
                // A shadow tree's !important wins over its host's own.
                for &(at, _) in important.iter().filter(|&&(at, _)| self.rules[at].inner) {
                    out.apply_only(decls(at), Some(true));
                }
            }
        };
        // Animations run above normal declarations, below !important ones.
        if !self.keyframes.is_empty() {
            let mut probe = out.clone();
            apply_important(&mut probe);
            let found = |name: &str| self.keyframes_named(name, around.tree);
            out.apply_only(&crate::animation::effect(&probe, &found), Some(false));
        }
        apply_important(&mut out);
        // ::first-line and ::first-letter: the start of the element's text.
        if !first_text.is_empty() {
            let variant = |kind: First, mut c: Computed| {
                let mut own: Vec<(usize, u32)> =
                    first_text.iter().copied().filter(|&(at, _)| self.rules[at].first == Some(kind)).collect();
                if own.is_empty() {
                    return None;
                }
                own.sort_unstable_by_key(|&(at, near)| self.rules[at].normal_rank(ranks, near));
                for &(at, _) in &own {
                    c.apply_only(&decls(at).only(FIRST_TEXT), Some(false));
                }
                own.sort_unstable_by_key(|&(at, near)| self.rules[at].important_rank(ranks, near));
                for &(at, _) in &own {
                    c.apply_only(&decls(at).only(FIRST_TEXT), Some(true));
                }
                Some(c)
            };
            let line = variant(First::Line, out.plain());
            let letter = variant(First::Letter, Computed::default());
            out.set_first_text(css::FirstText { line, letter });
        }
        // Reading the values, for every element, is paid for too.
        self.meter.spend(out.weight() / 32);
        out
    }

    /// The roots of `@scope` rule `id` an element is in, nearest first, with
    /// how many steps up each is: elements matching the scope's root, not
    /// past an element matching its limit on the way.
    fn scope_roots(&self, id: u32, node: &Node<'_>) -> Vec<(OpaqueElement, u32)> {
        let scope = &self.scopes[id as usize];
        let mut caches = SelectorCaches::default();
        let quirks = if self.quirks { QuirksMode::Quirks } else { QuirksMode::NoQuirks };
        let mut matches = |list: &SelectorList<MailImpl>, at: &Node<'_>| {
            let mut context = MatchingContext::new(
                MatchingMode::Normal,
                None,
                &mut caches,
                quirks,
                NeedsSelectorFlags::No,
                MatchingForInvalidation::No,
            );
            matching::matches_selector_list(list, at, &mut context)
        };
        let (mut out, mut blocked, mut steps) = (vec![], false, 0);
        let mut at = Some(*node);
        while let Some(a) = at {
            let is_root = match &scope.root {
                ScopeRoot::Selectors(list) => matches(list, &a),
                ScopeRoot::Element(id) => a.e.id() == *id,
            };
            if is_root && !blocked {
                out.push((a.opaque(), steps));
            }
            if scope.limit.as_ref().is_some_and(|l| matches(l, &a)) {
                blocked = true;
            }
            at = a.parent_element();
            steps += 1;
        }
        out
    }

    /// Does the `@container` query `id` (and those it is inside) hold for
    /// an element where `around` says? `cache` keeps the answers for it.
    fn container_holds(&self, id: u32, around: &Around<'_>, cache: &mut HashMap<u32, bool>) -> bool {
        if let Some(&holds) = cache.get(&id) {
            return holds;
        }
        let q = &self.queries[id as usize];
        let holds = self.meter.spend(1)
            && q.up.is_none_or(|up| self.container_holds(up, around, cache))
            && self.query_holds(q, around);
        cache.insert(id, holds);
        holds
    }

    /// One `@container` query. It asks about the nearest container around
    /// the element that has its name and can tell the sizes asked about;
    /// with none, it does not hold. A style query without a name asks
    /// about the parent.
    fn query_holds(&self, q: &Query, around: &Around<'_>) -> bool {
        let sized = q.width || q.height;
        let mut found = None;
        if sized || q.name.is_some() {
            found = around.containers.iter().rev().find(|c| {
                q.name.as_ref().is_none_or(|n| c.names.contains(n))
                    && (!q.width || c.size.is_some())
                    && (!q.height || c.size == Some(true))
            });
            if found.is_none() {
                return false;
            }
        }
        let Some(condition) = &q.condition else { return true };
        if !self.meter.spend(condition.len() / 8 + 1) {
            return false;
        }
        let vars = found.map_or(&around.vars, |c| &c.vars);
        let (mut width, height) = found.map_or(((0.0, 0.0), (0.0, 0.0)), |c| (c.width, c.height));
        let open = width.0 < width.1;
        if open && let Some(w) = self.probe.get() {
            let w = w.clamp(width.0, width.1);
            width = (w, w);
        }
        let sized = found.is_none_or(|c| c.sized);
        let font = found.map_or(16.0, |c| c.font);
        // A size the page leaves open: taken to hold in the first window,
        // not in the second, so what either way hides is hidden.
        let assume = *self.window() == css::WINDOWS[0];
        let mut needs = (false, false);
        css::container_holds(condition, &mut |probe| match probe {
            // A box containment does not apply to: its size is unknown.
            css::Probe::Size(f) => {
                let holds = css::size_feature_holds(f, width, height, &mut needs, assume, font, None);
                if open && needs.0 {
                    let mut fonts = self.uncertain.borrow_mut();
                    if !fonts.contains(&font) && fonts.len() < 8 {
                        fonts.push(font);
                    }
                }
                holds.filter(|_| sized)
            }
            css::Probe::Style(f) => css::style_holds(f, &|name| css::Vars::peek(vars, name), 0),
        }) == Some(true)
    }

    /// The keyframes named `name` for an element in `tree`.
    fn keyframes_named(&self, name: &str, tree: Option<NodeId>) -> Option<Keyframes> {
        let rules = self.keyframes.get(name)?;
        let ranks = self.ranks();
        [tree, None].iter().find_map(|tree| {
            rules
                .iter()
                .filter(|r| r.tree == *tree)
                .max_by_key(|r| (r.layer.map_or(u32::MAX, |l| ranks[l as usize]), r.position))
                .map(|r| r.frames.clone())
        })
    }
}

/// What matching a selector may cost: its parts, with those of the
/// selectors inside it (`:is()`, `:not()`, `:has()`, a nested rule's `&`),
/// counted down from `left` (left at 0 when there are more).
fn weight(s: &Selector<MailImpl>, left: &mut usize, depth: usize) {
    if depth > 32 {
        *left = 0;
        return;
    }
    for c in s.iter_raw_match_order() {
        if *left == 0 {
            return;
        }
        *left -= 1;
        match c {
            Component::Is(l) | Component::Where(l) | Component::Negation(l) => {
                for s in l.slice() {
                    weight(s, left, depth + 1);
                }
            }
            Component::Has(relative) => {
                for r in relative.iter() {
                    weight(&r.selector, left, depth + 1);
                }
            }
            Component::NthOf(nth) => {
                for s in nth.selectors() {
                    weight(s, left, depth + 1);
                }
            }
            Component::Host(Some(s)) | Component::Slotted(s) => weight(s, left, depth + 1),
            _ => {}
        }
    }
}

/// What a selector requires of the element it styles, to file it by.
enum Key {
    Id(String),
    Class(String),
    Tag(String),
    Any,
}

impl Key {
    fn of(selector: &Selector<MailImpl>) -> Key {
        let mut key = Key::Any;
        // The rightmost compound selector, which the element itself matches.
        for c in selector.iter() {
            match c {
                Component::ID(id) => return Key::Id(id.0.to_ascii_lowercase().to_string()),
                Component::Class(c) if !matches!(key, Key::Class(_)) => {
                    key = Key::Class(c.0.to_ascii_lowercase().to_string())
                }
                Component::LocalName(l) if matches!(key, Key::Any) => key = Key::Tag(l.lower_name.0.to_string()),
                _ => {}
            }
        }
        key
    }
}

/// Does an `@supports` condition hold in WebKit on a Mac? Declarations
/// are judged by property and value ([`css::declaration_supported`]),
/// `selector()` by whether WebKit would parse it; `not`, `and`, `or` and
/// parentheses are followed. What is not understood does not hold.
pub fn supports_applies(condition: &str) -> bool {
    let Some(text) = css::normalize(condition) else { return false };
    let Some(items) = css::items(&text) else { return false };
    css::condition(&items, &|x| supports_one(x, 0))
}

fn supports_one(item: &css::Item<'_>, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    match item {
        css::Item::Call("selector", inner) => {
            // One complex selector, every part of it understood.
            let mut input = Parser::new(inner);
            let strict = SelParser { nesting: true, strict: true, ..Default::default() };
            !inner.is_empty()
                && !too_nested(inner)
                && SelectorList::parse(&strict, &mut input, ParseRelative::No).is_ok_and(|l| l.slice().len() == 1)
                && input.is_exhausted()
        }
        css::Item::Call("font-format", f) => {
            matches!(f.trim().trim_matches('"'), "collection" | "opentype" | "truetype" | "woff" | "woff2")
        }
        css::Item::Call("font-tech", t) => matches!(
            t.trim(),
            "color-colrv0"
                | "color-sbix"
                | "color-svg"
                | "features-aat"
                | "features-opentype"
                | "palettes"
                | "variations"
        ),
        css::Item::Group(inner) => {
            if inner.starts_with('(')
                || inner.starts_with("not ")
                || inner.starts_with("not(")
                || inner.starts_with("selector(")
            {
                return css::items(inner).is_some_and(|it| css::condition(&it, &|x| supports_one(x, depth + 1)));
            }
            match inner.split_once(':') {
                Some((prop, value)) => css::declaration_supported(prop, value),
                None => false,
            }
        }
        _ => false,
    }
}

/// Nesting deeper than any real mail's: brackets in a selector or a
/// condition more than 32 levels deep. Such a sheet's text is withheld.
fn too_nested(text: &str) -> bool {
    let (mut depth, mut max, mut quote, mut escaped) = (0i32, 0i32, 0u8, false);
    for c in text.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, c) {
            (_, b'\\') => escaped = true,
            (0, b'"' | b'\'') => quote = c,
            (q, c) if q != 0 && c == q => quote = 0,
            (0, b'(' | b'[') => {
                depth += 1;
                max = max.max(depth);
            }
            (0, b')' | b']') => depth -= 1,
            _ => {}
        }
    }
    max > 32
}

/// What an at-rule's prelude decided.
enum At {
    /// `@media`, `@supports`: whether its rules apply.
    Block(bool),
    /// `@container`: its query, in [`Sheet::queries`].
    Container(u32),
    /// A cascade layer, by its full name; anonymous (`@layer {…}`) or not.
    Layer(String, bool),
    /// `@layer a, b;`: layers named, in this order.
    Layers(Vec<String>),
    /// `@scope`: in [`Sheet::scopes`].
    Scope(u32),
    Keyframes(String),
    /// Anything else (`@font-face`, `@-moz-document`…): its block is not read.
    Skip,
}

/// Where the rules being read stand.
#[derive(Clone, Default)]
struct Context {
    layer: Option<u32>,
    /// Full name of the layer, for the names of layers inside it.
    layer_name: String,
    /// The selectors of the style rule (CSS nesting) or `@scope` root the
    /// rules are inside.
    parent: Option<SelectorList<MailImpl>>,
    /// Inside a style rule: declarations belong to `parent`.
    in_rule: bool,
    /// Inside `@scope`: in [`Sheet::scopes`].
    scope: Option<u32>,
    /// Right inside `@scope`, not in a style rule there: a selector that
    /// names `:scope` (or `&`) is not made relative to the root.
    scope_top: bool,
    container: Option<u32>,
    depth: usize,
    /// The element the `<style>` is in.
    owner: Option<NodeId>,
    /// The shadow tree the sheet styles, its host, and the host's tree.
    tree: Option<NodeId>,
    host: Option<NodeId>,
    host_tree: Option<NodeId>,
    /// The sheet's `@namespace` rules.
    namespaces: Rc<Namespaces>,
}

/// Reads a style sheet, or a block of one, into a [`Sheet`]; inside a
/// style rule, also its declarations.
struct Reader<'s> {
    sheet: &'s mut Sheet,
    ctx: Context,
    decls: Decls,
    reader: css::DeclReader,
    /// The place in the cascade of the declarations before any nested rule.
    first: usize,
    /// A nested rule was met: the declarations after it come after it.
    after_rule: bool,
    /// Rules other than `@import` and `@namespace` were met: an
    /// `@namespace` after them is ignored.
    rules_seen: bool,
}

/// The rest of a parser's input, as written.
fn rest<'i>(input: &mut Parser<'i>) -> &'i str {
    let start = input.position();
    while input.next().is_ok() {}
    input.slice_from(start)
}

impl<'s> Reader<'s> {
    fn new(sheet: &'s mut Sheet, ctx: Context, first: usize) -> Reader<'s> {
        let quirks = sheet.quirks;
        Reader {
            sheet,
            ctx,
            decls: Decls::new(),
            reader: css::DeclReader::new(quirks),
            first,
            after_rule: false,
            rules_seen: false,
        }
    }

    fn selectors(&self, nesting: bool) -> SelParser<'_> {
        SelParser { nesting, host: self.ctx.host, namespaces: Some(&self.ctx.namespaces), strict: false }
    }

    /// `:where(:scope)`: what the rules of `@scope` are relative to.
    fn scope_parent(&mut self) -> SelectorList<MailImpl> {
        self.sheet
            .scope_parent
            .get_or_insert_with(|| {
                let mut input = Parser::new(":where(:scope)");
                SelectorList::parse(&SelParser::default(), &mut input, ParseRelative::No)
                    .unwrap_or_else(|_| SelectorList::scope())
            })
            .clone()
    }

    /// The declarations read so far, as a rule for the selectors of the
    /// style rule they are in.
    fn flush(&mut self) {
        if !self.ctx.in_rule {
            return;
        }
        let Some(parent) = self.ctx.parent.clone() else { return };
        let mut decls = std::mem::take(&mut self.decls);
        if self.reader.hostile() {
            decls.mark_hostile();
        }
        if decls.is_empty() && !decls.hostile() {
            return;
        }
        let position = if self.after_rule { self.sheet.position() } else { self.first };
        self.sheet.add_rule(&parent, decls, &self.ctx, position);
    }

    /// A rule inside this one begins: the declarations before it come
    /// before it in the cascade, those after it after it.
    fn nested_rule(&mut self) {
        if self.ctx.in_rule {
            self.flush();
            self.after_rule = true;
        }
    }

    /// Read a block with `ctx`: its rules, and inside a style rule its
    /// declarations.
    fn read_block(&mut self, ctx: Context, input: &mut Parser<'_>) {
        if ctx.depth > MAX_NESTING {
            // WebKit would apply rules this deep: they cannot be left out.
            self.sheet.meter.over.set(true);
            return;
        }
        let first = self.sheet.position();
        let in_rule = ctx.in_rule;
        let mut inner = Reader::new(&mut *self.sheet, ctx, first);
        if in_rule {
            for _ in RuleBodyParser::new(input, &mut inner) {}
        } else {
            for _ in StyleSheetParser::new(input, &mut inner) {}
        }
        inner.flush();
    }

    /// The full name of a layer named here; a new one when anonymous.
    fn layer_name(&mut self, name: Option<&str>) -> String {
        let name = match name {
            Some(n) => n.to_string(),
            None => format!("#{}", self.sheet.position()),
        };
        if self.ctx.layer_name.is_empty() { name } else { format!("{}.{name}", self.ctx.layer_name) }
    }

    fn layer_prelude(&mut self, text: &str) -> Result<At, ParseError<()>> {
        let text = css::normalize_keep(text).ok_or(ParseError::custom(()))?;
        if text.is_empty() {
            return Ok(At::Layer(self.layer_name(None), true));
        }
        let ident = |s: &str| {
            s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '-' || !c.is_ascii())
                && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || !c.is_ascii())
        };
        let mut names = vec![];
        for name in text.split(',').map(str::trim) {
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() > 16 || !parts.iter().all(|p| ident(p)) {
                return Err(ParseError::custom(()));
            }
            names.push(self.layer_name(Some(name)));
        }
        Ok(if names.len() == 1 { At::Layer(names.remove(0), false) } else { At::Layers(names) })
    }

    /// `@container [name] [query]`.
    fn container_prelude(&mut self, text: &str) -> At {
        let Some(text) = css::normalize_keep(text) else { return At::Block(false) };
        let named = |w: &str| {
            !matches!(w.to_ascii_lowercase().as_str(), "not" | "and" | "or" | "none")
                && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '-' || !c.is_ascii())
                && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || !c.is_ascii())
        };
        let (name, condition) = match text.split_once(' ') {
            Some((first, rest)) if named(first) => (Some(first), Some(rest.trim())),
            None if named(&text) => (Some(text.as_str()), None),
            _ => (None, Some(text.as_str())),
        };
        let mut needs = (false, false);
        if let Some(c) = condition {
            let breakpoints = &mut self.sheet.breakpoints;
            let read = css::container_holds(c, &mut |probe| match probe {
                css::Probe::Size(f) => {
                    css::size_feature_holds(f, (0.0, 1e6), (0.0, 1e6), &mut needs, true, 16.0, Some(&mut *breakpoints))
                }
                css::Probe::Style(f) => css::style_holds(f, &|_| None, 0),
            });
            if read.is_none() {
                return At::Block(false);
            }
        }
        if self.sheet.queries.len() >= MAX_LAYERS {
            self.sheet.meter.over.set(true);
            return At::Block(false);
        }
        // Containers are as large as the window lets them be.
        self.sheet.window_dependent |= needs.0 || needs.1;
        self.sheet.queries.push(Query {
            name: name.map(String::from),
            condition: condition.map(String::from),
            width: needs.0,
            height: needs.1,
            up: self.ctx.container,
        });
        At::Container(self.sheet.queries.len() as u32 - 1)
    }

    /// `@namespace [prefix] url(…);`, taken only before other rules.
    fn namespace(&mut self, input: &mut Parser<'_>, top: bool) {
        let mut read = || -> Option<(Option<String>, Namespace)> {
            let prefix = input.try_parse(|i| i.expect_ident().map(|p| p.to_string())).ok();
            let url = match input.next().ok()?.clone() {
                cssparser::Token::QuotedString(u) | cssparser::Token::UnquotedUrl(u) => u.to_string(),
                cssparser::Token::Function(f) if f.eq_ignore_ascii_case("url") => {
                    input.parse_nested_block(|i| Ok::<_, ParseError<()>>(i.expect_string()?.to_string())).ok()?
                }
                _ => return None,
            };
            input.expect_exhausted().ok()?;
            Some((prefix, Namespace::from(url)))
        };
        let Some((prefix, url)) = read() else { return };
        if !top || self.rules_seen {
            return;
        }
        let mut ns = (*self.ctx.namespaces).clone();
        match prefix {
            Some(p) => ns.prefixes.push((p, url)),
            None => ns.default = Some(url),
        }
        self.ctx.namespaces = Rc::new(ns);
    }

    /// `@scope [(root)] [to (limit)]`. Without a root, the scope is the
    /// element the `<style>` is in (in a shadow tree, its host).
    fn scope_prelude(&mut self, input: &mut Parser<'_>) -> Result<At, ParseError<()>> {
        fn group(input: &mut Parser<'_>) -> Result<String, ParseError<()>> {
            input.expect_parenthesis_block()?;
            input.parse_nested_block(|i| Ok(rest(i).to_string()))
        }
        let root = input.try_parse(group).ok();
        let limit = match input.try_parse(|i| i.expect_ident_matching("to")) {
            Ok(()) => Some(group(input)?),
            Err(_) => None,
        };
        input.expect_exhausted()?;
        if root.as_deref().is_some_and(too_nested) || limit.as_deref().is_some_and(too_nested) {
            self.sheet.meter.over.set(true);
            return Err(ParseError::custom(()));
        }
        let parser = self.selectors(false);
        let parse = |text: &str| -> Result<SelectorList<MailImpl>, ParseError<()>> {
            let mut i = Parser::new(text);
            let list = SelectorList::parse(&parser, &mut i, ParseRelative::No).map_err(|_| ParseError::custom(()))?;
            i.expect_exhausted()?;
            Ok(list)
        };
        let root = match root {
            Some(r) => ScopeRoot::Selectors(parse(&r)?),
            None => match self.ctx.owner {
                Some(owner) => ScopeRoot::Element(owner),
                None => return Ok(At::Skip),
            },
        };
        let limit = match limit {
            Some(l) => Some(parse(&l)?),
            None => None,
        };
        if self.sheet.scopes.len() >= MAX_LAYERS {
            self.sheet.meter.over.set(true);
            return Ok(At::Skip);
        }
        self.sheet.scopes.push(Scope { root, limit });
        Ok(At::Scope(self.sheet.scopes.len() as u32 - 1))
    }
}

/// Does a selector name `:scope`, at any depth?
fn names_scope(s: &Selector<MailImpl>, depth: usize) -> bool {
    depth < 32
        && s.iter_raw_match_order().any(|c| match c {
            Component::Scope => true,
            Component::Is(l) | Component::Where(l) | Component::Negation(l) => {
                l.slice().iter().any(|s| names_scope(s, depth + 1))
            }
            Component::Has(relative) => relative.iter().any(|r| names_scope(&r.selector, depth + 1)),
            _ => false,
        })
}

/// A nested rule's selectors made whole (CSS nesting): `.x` inside `.w` is
/// `.w .x`, `&:hover` is `.w:hover`, `> p` is `.w > p`. Right inside
/// `@scope` (`scope_top`), one naming `:scope` stays as written. `None` if
/// one is invalid: WebKit drops the rule.
fn nested(
    members: &[String],
    parent: &SelectorList<MailImpl>,
    parser: &SelParser<'_>,
    scope_top: bool,
) -> Option<SelectorList<MailImpl>> {
    let one = |text: &str, relative: ParseRelative| -> Option<Selector<MailImpl>> {
        let mut input = Parser::new(text);
        let list = SelectorList::parse(parser, &mut input, relative).ok()?;
        match list.slice() {
            [s] if input.is_exhausted() => Some(s.clone()),
            _ => None,
        }
    };
    let mut all = Vec::with_capacity(members.len());
    for member in members {
        let text = member.trim();
        // As written, it may name `&` (or, right in @scope, `:scope`);
        // otherwise it is relative: `& ` goes before it.
        let written =
            one(text, ParseRelative::No).filter(|s| s.has_parent_selector() || (scope_top && names_scope(s, 0)));
        all.push(match written {
            Some(s) => s,
            None => one(text, ParseRelative::ForNesting).filter(|s| s.has_parent_selector())?,
        });
    }
    Some(SelectorList::from_iter(all.into_iter()).replace_parent_selector(parent))
}

impl<'i> DeclarationParser<'i> for Reader<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<(), ParseError<()>> {
        if let Some(d) = self.reader.declaration(&name, input)? {
            self.decls.push(d);
        }
        Ok(())
    }
}

impl<'i> RuleBodyItemParser<'i, (), ()> for Reader<'_> {
    fn parse_declarations(&self) -> bool {
        self.ctx.in_rule
    }

    fn parse_qualified(&self) -> bool {
        true
    }
}

impl<'i> AtRuleParser<'i> for Reader<'_> {
    type Prelude = At;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i>) -> Result<At, ParseError<()>> {
        let top = !self.ctx.in_rule && self.ctx.parent.is_none();
        let name = name.to_ascii_lowercase();
        if name == "namespace" {
            self.namespace(input, top);
            return Ok(At::Skip);
        }
        if !matches!(name.as_str(), "import" | "charset" | "layer") {
            self.rules_seen = true;
        }
        let state = input.state();
        if too_nested(rest(input)) {
            self.sheet.meter.over.set(true);
            return Err(ParseError::custom(()));
        }
        input.reset(&state);
        Ok(match name.as_str() {
            "media" => {
                let text = rest(input);
                At::Block(self.sheet.media(text))
            }
            "supports" => At::Block(supports_applies(rest(input))),
            "container" => {
                let text = rest(input);
                self.container_prelude(text)
            }
            "layer" => {
                let text = rest(input);
                self.layer_prelude(text)?
            }
            "scope" => self.scope_prelude(input)?,
            "keyframes" | "-webkit-keyframes" if top => match css::normalize_keep(rest(input)) {
                Some(n) if !n.is_empty() && !n.contains(' ') => {
                    At::Keyframes(n.trim_matches(|c| c == '"' || c == '\'').to_string())
                }
                _ => At::Skip,
            },
            "import" => {
                let mut url: Option<String> = None;
                while let Ok(t) = input.next() {
                    match t {
                        cssparser::Token::UnquotedUrl(u) | cssparser::Token::QuotedString(u) if url.is_none() => {
                            url = Some(u.to_string())
                        }
                        cssparser::Token::Function(f) if f.eq_ignore_ascii_case("url") && url.is_none() => {
                            url = input
                                .parse_nested_block(|i| Ok::<_, ParseError<()>>(i.expect_string()?.to_string()))
                                .ok();
                        }
                        _ => {}
                    }
                }
                if let Some(u) = url {
                    match crate::util::data_url(&u) {
                        // A sheet written in the rule itself: read in place.
                        Some(css) if self.ctx.depth < 4 => {
                            let mut ctx = self.ctx.clone();
                            ctx.depth += 1;
                            let first = self.sheet.position();
                            let mut inner = Reader::new(&mut *self.sheet, ctx, first);
                            let mut input = Parser::new(&css);
                            for _ in StyleSheetParser::new(&mut input, &mut inner) {}
                        }
                        Some(_) => self.sheet.meter.over.set(true),
                        None => self.sheet.note_remote(&u),
                    }
                }
                At::Skip
            }
            _ => {
                rest(input);
                At::Skip
            }
        })
    }

    fn rule_without_block(&mut self, prelude: At, _start: &ParserState) -> Result<(), ()> {
        if !matches!(prelude, At::Layers(_) | At::Layer(..) | At::Skip) {
            self.rules_seen = true;
        }
        match prelude {
            At::Layers(names) => {
                for n in names {
                    self.sheet.layer(&n);
                }
            }
            At::Layer(n, false) => {
                self.sheet.layer(&n);
            }
            _ => {}
        }
        Ok(())
    }

    fn parse_block(&mut self, prelude: At, _start: &ParserState, input: &mut Parser<'i>) -> Result<(), ParseError<()>> {
        self.nested_rule();
        let mut ctx = self.ctx.clone();
        ctx.depth += 1;
        match prelude {
            At::Block(false) | At::Skip | At::Layers(_) => return Ok(()),
            At::Block(true) => {}
            At::Container(id) => ctx.container = Some(id),
            At::Layer(name, _) => {
                ctx.layer = Some(self.sheet.layer(&name));
                ctx.layer_name = name;
            }
            At::Scope(id) => {
                // Rules relative to the scope's root; declarations right
                // inside @scope are for the root itself.
                ctx.parent = Some(self.scope_parent());
                ctx.in_rule = true;
                ctx.scope = Some(id);
                ctx.scope_top = true;
            }
            At::Keyframes(name) => {
                let mut frames = Keyframes::default();
                let mut reader = FrameReader { frames: &mut frames, quirks: self.sheet.quirks };
                for _ in StyleSheetParser::new(input, &mut reader) {}
                if frames.hostile() || self.sheet.keyframes.len() >= MAX_LAYERS {
                    self.sheet.meter.over.set(true);
                }
                let rule = KeyframesRule { frames, layer: ctx.layer, tree: ctx.tree, position: self.sheet.position() };
                self.sheet.keyframes.entry(name).or_default().push(rule);
                return Ok(());
            }
        }
        self.read_block(ctx, input);
        Ok(())
    }
}

impl<'i> QualifiedRuleParser<'i> for Reader<'_> {
    type Prelude = SelectorList<MailImpl>;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<SelectorList<MailImpl>, ParseError<()>> {
        self.rules_seen = true;
        let state = input.state();
        if too_nested(rest(input)) {
            // WebKit reads selectors this deep; they cannot be checked here.
            self.sheet.meter.over.set(true);
            return Err(ParseError::custom(()));
        }
        input.reset(&state);
        // An invalid selector anywhere in the list drops the whole rule.
        match self.ctx.parent.clone() {
            None => {
                let list = SelectorList::parse(&self.selectors(true), input, ParseRelative::No)
                    .map_err(|_| ParseError::custom(()))?;
                // `&` outside any rule is `:scope`: the root element.
                if list.slice().iter().any(|s| s.has_parent_selector()) {
                    return Ok(list.replace_parent_selector(&SelectorList::scope()));
                }
                Ok(list)
            }
            Some(parent) => {
                let members = input.parse_comma_separated(|i| Ok::<_, ParseError<()>>(rest(i).to_string()))?;
                nested(&members, &parent, &self.selectors(true), self.ctx.scope_top).ok_or(ParseError::custom(()))
            }
        }
    }

    fn parse_block(
        &mut self,
        list: SelectorList<MailImpl>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        self.nested_rule();
        let mut ctx = self.ctx.clone();
        ctx.parent = Some(list);
        ctx.in_rule = true;
        ctx.scope_top = false;
        ctx.depth += 1;
        self.read_block(ctx, input);
        Ok(())
    }
}

/// Reads the frames of `@keyframes`.
struct FrameReader<'k> {
    frames: &'k mut Keyframes,
    quirks: bool,
}

impl<'i> AtRuleParser<'i> for FrameReader<'_> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for FrameReader<'_> {
    type Prelude = Vec<f32>;
    type QualifiedRule = ();
    type Error = ();

    /// `from`, `to` and percentages: the offsets of a keyframe.
    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Vec<f32>, ParseError<()>> {
        input.parse_comma_separated(|i| match i.next()?.clone() {
            cssparser::Token::Ident(w) if w.eq_ignore_ascii_case("from") => Ok(0.0),
            cssparser::Token::Ident(w) if w.eq_ignore_ascii_case("to") => Ok(1.0),
            cssparser::Token::Percentage { unit_value, .. } if (0.0..=1.0).contains(&unit_value) => Ok(unit_value),
            _ => Err(ParseError::custom(())),
        })
    }

    fn parse_block(
        &mut self,
        offsets: Vec<f32>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let decls = Decls::read(input, self.quirks);
        if self.frames.len() < 1000 {
            self.frames.add(&offsets, &decls);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::Html;

    /// The computed value of `prop` for the element with id `id`.
    fn value(html: &str, css: &str, id: &str, prop: &str) -> Option<String> {
        let doc = Html::parse_document(html);
        let mut s = Sheet::new(format!("{:?}", doc.quirks_mode) == "Quirks", css::WINDOWS[0]);
        s.add_css(css);
        let e = doc.tree.nodes().filter_map(ElementRef::wrap).find(|e| e.value().id() == Some(id))?;
        s.compute(e, e.value().attr("style")).get(prop).map(str::to_string)
    }

    const DOC: &str = r#"<!doctype html><html><body><div class="wrap"><span class="hide" id="a">a</span></div>
        <span class="hide" id="b">b</span><div class="a"><p class="b" id="c">c</p><p class="b" id="d">d</p></div>
        <input type="checkbox" id="t"><div class="content" id="e">e</div><table><tr><td><p id="f">f</p></td></tr></table></body></html>"#;

    #[test]
    fn selectors_hold_where_they_say() {
        let css = ".wrap .hide{display:none} .a > .b + .b{display:none} #t:checked ~ .content{display:block}
                   .content{display:none} td p{font-size:0} p.b{font-size:14px}";
        assert_eq!(value(DOC, css, "a", "display").as_deref(), Some("none"));
        assert_eq!(value(DOC, css, "b", "display"), None, "outside .wrap");
        assert_eq!(value(DOC, css, "c", "display"), None, "no .b before it");
        assert_eq!(value(DOC, css, "d", "display").as_deref(), Some("none"));
        assert_eq!(value(DOC, css, "e", "display").as_deref(), Some("none"), "the box is not checked");
        assert_eq!(value(DOC, css, "f", "font-size").as_deref(), Some("0"));
        assert_eq!(value(DOC, css, "c", "font-size").as_deref(), Some("14px"), "more specific");
    }

    #[test]
    fn rules_mail_does_not_apply() {
        let hide = ".hide{display:none}";
        for css in [
            ".nope + .hide{display:inline}",
            ".hide, ::-moz-selection{display:inline}",
            ".hide, .x:-moz-any(p){display:inline}",
            ".hide:hover{display:inline}",
            "@supports (bogus:1){.hide{display:inline}}",
            "@-moz-document url-prefix(){.hide{display:inline}}",
            "@media (min-height:100000px){.hide{display:inline}}",
            "@media (monochrome){.hide{display:inline}}",
            "@media (max-device-width:1024px){.hide{display:inline}}",
            "[data-ogsc] .hide{display:inline}",
            ".hide{display:bogus}",
        ] {
            let all = format!("{hide} {css}");
            let got = value(DOC, &all, "b", "display");
            assert!(got.as_deref() == Some("none"), "{css}: {got:?}");
        }
        // Stray characters spoil one rule, never the rest of the sheet.
        for css in [
            ".a{font-family:\"x\n} .hide{display:none}",
            "} .lost{x:y} .hide{display:none}",
            ".q{content:\"}\"} .hide{display:none}",
            ".q{content:\"/*\"} .hide{display:none} /* */",
            ".q\\{x{color:red} .hide{display:none}",
        ] {
            assert_eq!(value(DOC, css, "b", "display").as_deref(), Some("none"), "{css}");
        }
        assert_eq!(
            value(DOC, "@layer x{.hide{display:none}} .hide{display:inline}", "b", "display").as_deref(),
            Some("inline")
        );
        // With !important, a layer wins over the rules outside layers.
        let css = "@layer x{.hide{display:none !important}} .hide{display:inline !important}";
        assert_eq!(value(DOC, css, "b", "display").as_deref(), Some("none"));
        let html = r#"<!doctype html><span class="hide" id="s" style="display:inline !important">s</span>"#;
        assert_eq!(value(html, css, "s", "display").as_deref(), Some("inline"), "the style attribute wins over all");
        assert_eq!(
            value(DOC, "@supports (display:grid){.hide{display:none}}", "b", "display").as_deref(),
            Some("none")
        );
        assert_eq!(value(DOC, ".hide:not(.x){display:none}", "b", "display").as_deref(), Some("none"));
        assert_eq!(value(DOC, "div:has(> .hide){display:none}", "a", "display"), None);
    }

    #[test]
    fn quirks_mode_matches_classes_whatever_their_case() {
        let quirky = r#"<html><body><span class="hide" id="b">b</span></body></html>"#;
        assert_eq!(value(quirky, ".HIDE{display:none}", "b", "display").as_deref(), Some("none"));
        assert_eq!(value(DOC, ".HIDE{display:none}", "b", "display"), None);
    }

    #[test]
    fn hostile_sheets_cost_a_bounded_time() {
        let t = std::time::Instant::now();
        // A selector of a hundred thousand classes, and many elements.
        let html = format!("<body>{}</body>", "<i class=x>x</i>".repeat(20_000));
        let doc = Html::parse_document(&html);
        let mut s = Sheet::new(false, css::WINDOWS[0]);
        s.add_css(&format!("{}{{display:none}}", ".x".repeat(100_000)));
        for e in doc.tree.nodes().filter_map(ElementRef::wrap) {
            let _ = s.compute(e, None);
        }
        assert!(t.elapsed() < crate::util::time_limit(10), "{:?}", t.elapsed());
        assert!(s.overflowed());
        // Deep nesting and selectors that try every way up a deep tree.
        let deep = format!("<body>{}<b id=z>z</b>{}</body>", "<div>".repeat(300), "</div>".repeat(300));
        let doc = Html::parse_document(&deep);
        let mut s = Sheet::new(false, css::WINDOWS[0]);
        s.add_css(&format!("{} b{{display:none}} {}", "div ".repeat(30), "@media screen{".repeat(50_000)));
        s.add_css(&":has(:has(:has(*))){color:red}".repeat(100));
        let t = std::time::Instant::now();
        for e in doc.tree.nodes().filter_map(ElementRef::wrap) {
            let _ = s.compute(e, None);
        }
        assert!(t.elapsed() < crate::util::time_limit(10), "{:?}", t.elapsed());
        // Too many rules.
        let mut s = Sheet::new(false, css::WINDOWS[0]);
        s.add_css(&".y{display:none}".repeat(30_000));
        assert!(s.overflowed());
    }

    #[test]
    fn nesting_layers_and_queries_cost_a_bounded_time() {
        let html = format!("<body>{}</body>", "<div class=a><p class=b>x</p></div>".repeat(2_000));
        let doc = Html::parse_document(&html);
        let sheets = [
            // Every level names its parent ten times: a hundred million
            // selectors, were they copied out.
            format!("{}.z{{display:none}}{}", ".a{& & & & & & & & & &{".repeat(16), "}}".repeat(16)),
            // Nesting past the limit is not left out: the text is withheld.
            format!("{}.b{{display:none}}{}", ".a{".repeat(40), "}".repeat(40)),
            (0..30_000).map(|i| format!("@layer l{i}{{.b{{color:red}}}}")).collect(),
            (0..30_000).map(|i| format!("@container q{i} (min-width:{i}px){{.b{{color:red}}}}")).collect(),
            (0..20_000).map(|i| format!("@keyframes k{i}{{to{{opacity:0}}}}")).collect(),
            format!("@layer {};", (0..100).map(|_| "a.".repeat(20) + "b").collect::<Vec<_>>().join(",")),
            format!("@scope (.a) to (.b){{{}}}", ".b{color:red}".repeat(20_000)),
        ];
        for css in sheets {
            let t = std::time::Instant::now();
            let mut s = Sheet::new(false, css::WINDOWS[0]);
            s.add_css(&css);
            for e in doc.tree.nodes().filter_map(ElementRef::wrap) {
                let _ = s.compute(e, None);
            }
            assert!(t.elapsed() < crate::util::time_limit(5), "{:?}: {}", t.elapsed(), &css[..60]);
        }
        let mut s = Sheet::new(false, css::WINDOWS[0]);
        s.add_css(&format!("{}.b{{display:none}}{}", ".a{".repeat(40), "}".repeat(40)));
        assert!(s.overflowed(), "rules nested past the limit");
    }

    #[test]
    fn layers_nest_and_order_as_in_css() {
        let html = r#"<!doctype html><p class="x" id="p">p</p>"#;
        let display = |css: &str| value(html, css, "p", "display");
        // A layer's own rules come after the layers inside it.
        assert_eq!(display("@layer a{.x{display:none} @layer b{.x.x{display:block}}}").as_deref(), Some("none"));
        // `a.b` is `b` inside `a`, wherever it is named.
        assert_eq!(display("@layer a{.x{display:none}} @layer a.b{.x.x{display:block}}").as_deref(), Some("none"));
        // Order of first naming, statements included.
        assert_eq!(
            display("@layer b, a; @layer a{.x{display:none}} @layer b{.x.x{display:block}}").as_deref(),
            Some("none")
        );
        // Anonymous layers are each their own.
        assert_eq!(display("@layer{.x.x{display:block}} @layer{.x{display:none}}").as_deref(), Some("none"));
        // !important: the order reversed.
        assert_eq!(
            display("@layer a{.x{display:none!important}} @layer b{.x{display:block!important}}").as_deref(),
            Some("none")
        );
        // Not a layer name: the rule is dropped.
        assert_eq!(display("@layer 1a{.x{display:none}}"), None);
    }

    #[test]
    fn nested_rules_read_as_written_out() {
        let html = r#"<!doctype html><div class="w"><p class="x" id="p">p</p></div><p class="x" id="q">q</p>"#;
        let d = |css: &str, id: &str| value(html, css, id, "display");
        assert_eq!(d(".w{.x{display:none}}", "p").as_deref(), Some("none"));
        assert_eq!(d(".w{.x{display:none}}", "q"), None);
        assert_eq!(d(".w{> .x{display:none}}", "p").as_deref(), Some("none"));
        assert_eq!(d(".x{.w &{display:none}}", "p").as_deref(), Some("none"));
        assert_eq!(d(".x{.w &{display:none}}", "q"), None);
        assert_eq!(d(".w{@media screen{.x{display:none}}}", "p").as_deref(), Some("none"));
        assert_eq!(d(".w{@media print{.x{display:none}}}", "p"), None);
        // Specificity of `&` is its parent's: .w .x (0,2,0) beats .x (0,1,0).
        assert_eq!(d(".w{.x{display:none}} .x{display:block}", "p").as_deref(), Some("none"));
        // Declarations after a nested rule come after it.
        assert_eq!(d(".x{&{display:block} display:none}", "q").as_deref(), Some("none"));
        assert_eq!(d(".x{display:none; &{display:block}}", "q").as_deref(), Some("block"));
        // A declaration WebKit drops leaves the next one.
        assert_eq!(d(".x{color:bogus; display:none}", "q").as_deref(), Some("none"));
    }

    #[test]
    fn remote_style_sheets_are_noted() {
        let mut s = Sheet::new(false, css::WINDOWS[0]);
        s.add_css("@import url(\"https://fonts.googleapis.com/css?family=Roboto\"); .x{color:red}");
        assert_eq!(s.remote(), None, "a web font");
        s.add_css("@import 'styles.css';");
        assert_eq!(s.remote(), None, "relative: nothing to load");
        s.add_css("@import 'https://cdn.evil.example/x.css';");
        assert_eq!(s.remote(), Some("cdn.evil.example"));
    }
}
