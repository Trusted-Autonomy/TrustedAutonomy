import * as http from 'http';
import * as https from 'https';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { URL } from 'url';

/**
 * The human credential created by `ta login --file` (or TA_HUMAN_CREDENTIAL).
 * The daemon requires it for approve, deny, apply, commands and answers.
 * Returns '' when the user has not run `ta login`.
 */
export function humanCredential(): string {
  const fromEnv = (process.env.TA_HUMAN_CREDENTIAL || '').trim();
  if (fromEnv) {
    return fromEnv;
  }
  const dir = process.env.TA_USER_STATE_DIR || path.join(os.homedir(), '.config', 'ta', 'human');
  try {
    return fs.readFileSync(path.join(dir, 'credential'), 'utf8').trim();
  } catch {
    return '';
  }
}

/**
 * Advisory `X-TA-Actor-Context` value when this editor was launched from an
 * agent (same environment markers the `ta` CLI checks), else ''. The daemon
 * records it as `actor_context: agent_delegated` in the audit trail. It is not
 * a security control: an agent can unset the variables.
 */
export function actorContextHeader(env: NodeJS.ProcessEnv = process.env): string {
  const reasons: string[] = [];
  for (const name of [
    'TA_AGENT_CONTEXT',
    'TA_IS_STAGING',
    'TA_AGENT_ID',
    'CLAUDECODE',
    'CLAUDE_CODE_ENTRYPOINT',
  ]) {
    if (env[name]) {
      reasons.push(`${name} is set`);
    }
  }
  const mode = env.TA_CALLER_MODE;
  if (mode && mode !== 'normal') {
    reasons.push(`TA_CALLER_MODE=${mode}`);
  }
  if (env.TA_CHAT_MODE) {
    reasons.push('TA_CHAT_MODE is set');
  }
  if (reasons.length === 0) {
    return '';
  }
  const encoded = reasons
    .slice(0, 5)
    .map((r) => encodeURIComponent(r.slice(0, 60)).replace(/[!'()*]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase()));
  return `agent_delegated; reasons=${encoded.join(',')}`;
}

export interface HealthResponse {
  status: string;
  version: string;
  timestamp: string;
  plugins: string[];
}

export interface AgentInfo {
  agent_id: string;
  goal_id: string;
  tag: string;
  title: string;
  state: string;
  running_secs: number;
  active: boolean;
  vcs_state?: string;
  process_health?: string;
}

export interface ProjectStatus {
  project: string;
  version: string;
  daemon_version: string;
  current_phase?: { id: string; title: string; status: string };
  active_agents: AgentInfo[];
  pending_drafts: number;
  active_goals: number;
  total_goals: number;
  power_assertion_active: boolean;
  active_project_path?: string;
}

export interface DraftSummary {
  package_id: string;
  title: string;
  status: string;
  created_at: string;
  artifact_count: number;
  goal_id?: string;
}

export interface Artifact {
  resource_uri: string;
  change_type: string;
  diff_ref: string;
  rationale?: string;
}

export interface DraftPackage {
  package_id: string;
  created_at: string;
  goal: {
    goal_id: string;
    title: string;
    objective: string;
  };
  summary: {
    what_changed: string;
    why: string;
    impact: string;
  };
  changes: {
    artifacts: Artifact[];
  };
  status: string;
  plan_phase?: string;
  tag?: string;
  display_id?: string;
}

export interface CmdResponse {
  exit_code: number;
  stdout: string;
  stderr: string;
  background_key?: string;
}

function cfg<T>(key: string): T {
  return vscode.workspace.getConfiguration('ta').get<T>(key) as T;
}

export class TaClient {
  private get baseUrl(): string {
    return cfg<string>('daemonUrl') || 'http://127.0.0.1:7700';
  }

  private get token(): string {
    return cfg<string>('apiToken') || '';
  }

  private request<T>(
    method: string,
    path: string,
    body?: unknown,
    auth = true,
  ): Promise<T> {
    return new Promise((resolve, reject) => {
      const url = new URL(path, this.baseUrl);
      const isHttps = url.protocol === 'https:';
      const transport = isHttps ? https : http;

      const payload = body ? JSON.stringify(body) : undefined;
      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
        Accept: 'application/json',
      };
      if (auth && this.token) {
        headers['Authorization'] = `Bearer ${this.token}`;
      }
      const credential = auth ? humanCredential() : '';
      if (credential) {
        headers['X-TA-Human-Credential'] = credential;
      }
      const actorContext = auth ? actorContextHeader() : '';
      if (actorContext) {
        headers['X-TA-Actor-Context'] = actorContext;
      }
      if (payload) {
        headers['Content-Length'] = Buffer.byteLength(payload).toString();
      }

      const options: http.RequestOptions = {
        hostname: url.hostname,
        port: url.port || (isHttps ? 443 : 80),
        path: url.pathname + url.search,
        method,
        headers,
        timeout: 15_000,
      };

      const req = transport.request(options, (res) => {
        const chunks: Buffer[] = [];
        res.on('data', (c: Buffer) => chunks.push(c));
        res.on('end', () => {
          const text = Buffer.concat(chunks).toString();
          if (!res.statusCode || res.statusCode >= 400) {
            reject(new Error(`HTTP ${res.statusCode}: ${text.slice(0, 200)}`));
            return;
          }
          try {
            resolve(JSON.parse(text) as T);
          } catch {
            resolve(text as unknown as T);
          }
        });
      });

      req.on('error', reject);
      req.on('timeout', () => {
        req.destroy();
        reject(new Error('Request timed out'));
      });

      if (payload) {
        req.write(payload);
      }
      req.end();
    });
  }

  health(): Promise<HealthResponse> {
    return this.request<HealthResponse>('GET', '/health', undefined, false);
  }

  getStatus(): Promise<ProjectStatus> {
    return this.request<ProjectStatus>('GET', '/api/status');
  }

  listDrafts(): Promise<DraftSummary[]> {
    return this.request<DraftSummary[]>('GET', '/api/drafts');
  }

  getDraft(id: string): Promise<DraftPackage> {
    return this.request<DraftPackage>('GET', `/api/drafts/${id}`);
  }

  approveDraft(id: string): Promise<{ package_id: string; status: string; message: string }> {
    return this.request('POST', `/api/drafts/${id}/approve`, {});
  }

  denyDraft(id: string, reason: string): Promise<{ package_id: string; status: string; message: string }> {
    return this.request('POST', `/api/drafts/${id}/deny`, { reason });
  }

  runCommand(command: string): Promise<CmdResponse> {
    return this.request<CmdResponse>('POST', '/api/cmd', { command });
  }

  /** Open a Server-Sent Events stream. Returns the http.ClientRequest so the caller can abort it. */
  openEventStream(
    onEvent: (type: string, data: string) => void,
    onError: (err: Error) => void,
    since?: string,
    types?: string,
  ): http.ClientRequest {
    const url = new URL('/api/events', this.baseUrl);
    if (since) {
      url.searchParams.set('since', since);
    }
    if (types) {
      url.searchParams.set('types', types);
    }

    const isHttps = url.protocol === 'https:';
    const transport = isHttps ? https : http;

    const headers: Record<string, string> = {
      Accept: 'text/event-stream',
      'Cache-Control': 'no-cache',
    };
    if (this.token) {
      headers['Authorization'] = `Bearer ${this.token}`;
    }

    const options: http.RequestOptions = {
      hostname: url.hostname,
      port: url.port || (isHttps ? 443 : 80),
      path: url.pathname + url.search,
      method: 'GET',
      headers,
    };

    const req = transport.request(options, (res) => {
      let buffer = '';
      res.setEncoding('utf8');
      res.on('data', (chunk: string) => {
        buffer += chunk;
        const lines = buffer.split('\n');
        buffer = lines.pop() ?? '';

        let eventType = 'message';
        let data = '';

        for (const line of lines) {
          if (line.startsWith('event:')) {
            eventType = line.slice(6).trim();
          } else if (line.startsWith('data:')) {
            data = line.slice(5).trim();
          } else if (line === '' && data) {
            onEvent(eventType, data);
            eventType = 'message';
            data = '';
          }
        }
      });
      res.on('error', onError);
    });

    req.on('error', onError);
    req.end();
    return req;
  }
}
