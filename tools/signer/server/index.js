#!/usr/bin/env node
// Timbre Signer — a minimal MCP server (stdio) with one tool, timbre_signing.
// Copyright (c) 2026 Alejandro San Martin Lamas — https://github.com/alejandrosml (MIT)
'use strict';

const { execFile, spawn } = require('child_process');
const path = require('path');
const readline = require('readline');

const SCRIPT = path.join(__dirname, 'sign.sh');
const ACTIONS = ['status', 'prepare', 'install', 'sign'];
const TOOL = {
  name: 'timbre_signing',
  title: 'Timbre release signing',
  description:
    'Developer ID signing for Timbre on this Mac. Actions: ' +
    '"status" (what is set up, last signing log); ' +
    '"prepare" (create the private key in a private folder and return the certificate signing request, PEM); ' +
    '"install" (with "certificate": the base64 DER Developer ID Application certificate issued for that request; ' +
    'also takes an AuthKey_*.p8 from Downloads with KEY_ID and ISSUER from Downloads/timbre-notary.txt for notarization); ' +
    '"sign" (with "file": a timbre-X.Y.Z.mcpb in Downloads; signs, notarizes and writes timbre-X.Y.Z-signed.mcpb in the background, follow with status).',
  inputSchema: {
    type: 'object',
    properties: {
      action: { type: 'string', enum: ACTIONS },
      certificate: { type: 'string', description: 'For install: base64 of the .cer file (public data).' },
      file: { type: 'string', description: 'For sign: file name of a timbre-X.Y.Z.mcpb in Downloads.' },
    },
    required: ['action'],
    additionalProperties: false,
  },
  annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: true },
};

function send(msg) {
  process.stdout.write(JSON.stringify(msg) + '\n');
}

function result(id, text, isError = false) {
  send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text }], isError } });
}

function run(args, input) {
  return new Promise((resolve) => {
    const child = execFile('/bin/bash', [SCRIPT, ...args], { timeout: 180000, maxBuffer: 1 << 20 }, (err, stdout, stderr) => {
      const text = [stdout, stderr].filter((s) => s && s.trim()).join('\n').trim();
      resolve({ ok: !err, text: text || (err ? String(err.message || err) : 'ok') });
    });
    child.stdin.end(input === undefined ? '' : input); // never leave a reader waiting
  });
}

async function call(id, args) {
  const action = args && args.action;
  if (!ACTIONS.includes(action)) return result(id, `Unknown action. Use one of: ${ACTIONS.join(', ')}.`, true);
  if (action === 'sign') {
    const file = String(args.file || '');
    if (!/^timbre-\d+\.\d+\.\d+\.mcpb$/.test(file)) {
      return result(id, 'file must be the name of a timbre-X.Y.Z.mcpb in Downloads.', true);
    }
    // Notarization takes minutes: run detached, report through status.
    const child = spawn('/bin/bash', [SCRIPT, 'sign-run', file], { detached: true, stdio: 'ignore' });
    child.unref();
    return result(id, `Signing ${file} started. Call action "status" to follow it (notarization usually takes 1 to 10 minutes).`);
  }
  if (action === 'install') {
    const cert = String(args.certificate || '').replace(/\s+/g, '');
    if (cert && !/^[A-Za-z0-9+/=]+$/.test(cert)) return result(id, 'certificate must be base64.', true);
    const r = await run(['install'], cert);
    return result(id, r.text, !r.ok);
  }
  const r = await run([action]);
  return result(id, r.text, !r.ok);
}

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      return send({
        jsonrpc: '2.0',
        id,
        result: {
          protocolVersion: (params && params.protocolVersion) || '2025-06-18',
          capabilities: { tools: {} },
          serverInfo: { name: 'timbre-signer', title: 'Timbre Signer', version: '0.1.1' },
        },
      });
    case 'tools/list':
      return send({ jsonrpc: '2.0', id, result: { tools: [TOOL] } });
    case 'tools/call':
      if (!params || params.name !== TOOL.name) return result(id, 'Unknown tool.', true);
      return void call(id, params.arguments || {});
    case 'ping':
      return send({ jsonrpc: '2.0', id, result: {} });
    default:
      if (id !== undefined && id !== null) {
        send({ jsonrpc: '2.0', id, error: { code: -32601, message: `Method not found: ${method}` } });
      }
  }
});
