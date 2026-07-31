#!/usr/bin/env node

import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';

import { FACTORY_PROTOCOL_MANIFEST } from '../../../harness-client/dist/index.js';

if (process.argv[2] === 'protocol-manifest') {
  process.stdout.write(`${JSON.stringify(FACTORY_PROTOCOL_MANIFEST)}\n`);
  process.exit(0);
}

const threadId = 'pending-request-thread';
const turnId = 'pending-request-turn';
let fallthroughObserved = false;

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function turn(status, completed = false) {
  return {
    id: turnId,
    items: [],
    itemsView: { type: 'full' },
    status,
    error: null,
    startedAt: 1,
    completedAt: completed ? 2 : null,
    durationMs: completed ? 1 : null,
  };
}

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.method === 'initialized') return;

  if (message.method === 'initialize') {
    send({
      id: message.id,
      result: {
        userAgent: 'pending-request-fixture',
        codexHome: process.env.CODEX_HOME ?? '/tmp/pending-request-codex-home',
        platformFamily: process.platform,
        platformOs: process.platform,
      },
    });
    return;
  }

  if (message.method === 'thread/start') {
    send({
      id: message.id,
      result: {
        thread: {
          id: threadId,
          sessionId: 'pending-request-session',
          forkedFromId: null,
          parentThreadId: null,
          preview: '',
          ephemeral: false,
          section: null,
          sectionEnteredAt: null,
          modelProvider: 'fixture-provider',
          createdAt: 1,
          updatedAt: 1,
          recencyAt: 1,
          status: { type: 'idle' },
          path: null,
          cwd: message.params.cwd,
          cliVersion: 'fixture',
          source: 'appServer',
          threadSource: null,
          agentNickname: null,
          agentRole: null,
          gitInfo: null,
          name: null,
          turns: [],
        },
        model: 'fixture-model',
        modelProvider: 'fixture-provider',
        serviceTier: null,
        cwd: message.params.cwd,
        instructionSources: [],
        approvalPolicy: 'on-request',
        approvalsReviewer: 'user',
        sandbox: { type: 'dangerFullAccess' },
        reasoningEffort: null,
      },
    });
    return;
  }

  if (message.method === 'turn/start') {
    send({ id: message.id, result: { turn: turn('inProgress') } });
    send({
      id: 700,
      method: 'item/commandExecution/requestApproval',
      params: {
        threadId,
        turnId,
        itemId: 'pending-command',
        startedAtMs: 1,
        environmentId: null,
        command: 'git status',
        cwd: message.params.cwd,
      },
    });
    send({ id: 701, method: 'attestation/generate', params: {} });
    return;
  }

  if (message.id === 701) {
    assert.equal(message.error?.code, -32601, 'non-human request did not fall through');
    fallthroughObserved = true;
    return;
  }

  if (message.id === 700) {
    assert.equal(fallthroughObserved, true, 'human wait blocked the inbound reader');
    assert.equal(message.result?.decision, 'accept', 'approval resolution changed on the wire');
    send({
      method: 'serverRequest/resolved',
      params: { threadId, requestId: 700 },
    });
    send({
      method: 'turn/completed',
      params: { threadId, turn: turn('completed', true) },
    });
    return;
  }

  if (message.method === 'turn/interrupt') {
    send({ id: message.id, result: {} });
    return;
  }

  throw new Error(`pending-request fixture received unexpected message: ${line}`);
});
