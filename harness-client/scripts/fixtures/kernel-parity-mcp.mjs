import { appendFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const generation = process.env.KERNEL_PARITY_MCP_GENERATION;
const lifecyclePath = process.env.KERNEL_PARITY_MCP_LIFECYCLE_FILE;

if (!generation || !lifecyclePath) {
  throw new Error('kernel parity MCP fixture generation and lifecycle path are required');
}

appendFileSync(lifecyclePath, `${JSON.stringify({ event: 'started', generation, pid: process.pid })}\n`);

const resourceUri = 'fixture://kernel-parity/state';
const input = createInterface({ input: process.stdin, crlfDelay: Infinity });

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function result(id, value) {
  send({ jsonrpc: '2.0', id, result: value });
}

function error(id, code, message) {
  send({ jsonrpc: '2.0', id, error: { code, message } });
}

for await (const line of input) {
  if (line.trim().length === 0) continue;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    continue;
  }
  if (message.id === undefined || message.id === null) continue;

  switch (message.method) {
    case 'initialize':
      result(message.id, {
        protocolVersion: message.params?.protocolVersion ?? '2025-06-18',
        capabilities: {
          resources: { listChanged: false, subscribe: false },
          tools: { listChanged: false },
        },
        serverInfo: {
          name: 'kernel-parity-mcp',
          title: `Kernel Parity MCP ${generation}`,
          version: generation,
        },
      });
      break;
    case 'ping':
    case 'logging/setLevel':
      result(message.id, {});
      break;
    case 'resources/list':
      result(message.id, {
        resources: [{
          uri: resourceUri,
          name: 'kernel-parity-state',
          title: `Kernel parity state ${generation}`,
          description: 'Disposable deterministic MCP resource.',
          mimeType: 'text/plain',
        }],
      });
      break;
    case 'resources/templates/list':
      result(message.id, { resourceTemplates: [] });
      break;
    case 'resources/read':
      if (message.params?.uri !== resourceUri) {
        error(message.id, -32002, `unknown resource ${String(message.params?.uri)}`);
        break;
      }
      result(message.id, {
        contents: [{
          uri: resourceUri,
          mimeType: 'text/plain',
          text: `kernel-parity-resource:${generation}`,
        }],
      });
      break;
    case 'tools/list':
      result(message.id, {
        tools: [{
          name: 'echo_generation',
          title: 'Echo generation',
          description: 'Echo a message with the current fixture generation.',
          inputSchema: {
            type: 'object',
            properties: { message: { type: 'string' } },
            required: ['message'],
            additionalProperties: false,
          },
          annotations: { readOnlyHint: true },
        }],
      });
      break;
    case 'tools/call': {
      if (message.params?.name !== 'echo_generation') {
        error(message.id, -32602, `unknown tool ${String(message.params?.name)}`);
        break;
      }
      const echoed = message.params?.arguments?.message;
      if (typeof echoed !== 'string') {
        error(message.id, -32602, 'message must be a string');
        break;
      }
      result(message.id, {
        content: [{ type: 'text', text: `kernel-parity-echo:${echoed}:${generation}` }],
        structuredContent: { echoed, generation },
        isError: false,
      });
      break;
    }
    default:
      error(message.id, -32601, `method not found: ${String(message.method)}`);
  }
}
