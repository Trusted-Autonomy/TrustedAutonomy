import * as http from 'http';
import * as vscode from 'vscode';
import { TaClient } from './client';
import { GoalsProvider } from './goalsProvider';
import { DraftsProvider } from './draftsProvider';

interface TaEvent {
  id?: string;
  event_type?: string;
  timestamp?: string;
  payload?: {
    goal_id?: string;
    title?: string;
    state?: string;
    package_id?: string;
    draft_title?: string;
    message?: string;
    [key: string]: unknown;
  };
}

const GOAL_COMPLETE_STATES = new Set(['pr_ready', 'applied', 'failed', 'denied', 'completed']);
const INTERESTING_TYPES = [
  'goal_state_changed',
  'draft_ready',
  'draft_approved',
  'draft_denied',
  'goal_failed',
];

export class NotificationListener {
  private req?: http.ClientRequest;
  private reconnectTimer?: NodeJS.Timeout;
  private lastEventTimestamp?: string;
  private stopped = false;

  constructor(
    private readonly client: TaClient,
    private readonly goalsProvider: GoalsProvider,
    private readonly draftsProvider: DraftsProvider,
  ) {}

  start(): void {
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    this.req?.destroy();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
    }
  }

  private connect(): void {
    if (this.stopped) {
      return;
    }

    const typeFilter = INTERESTING_TYPES.join(',');

    this.req = this.client.openEventStream(
      (type, data) => this.onEvent(type, data),
      (err) => {
        // Only log reconnect issues when not intentionally stopped
        if (!this.stopped) {
          console.debug(`[TA] SSE error: ${err.message} — reconnecting in 15s`);
          this.scheduleReconnect(15_000);
        }
      },
      this.lastEventTimestamp,
      typeFilter,
    );
  }

  private onEvent(type: string, data: string): void {
    let event: TaEvent;
    try {
      event = JSON.parse(data) as TaEvent;
    } catch {
      return;
    }

    if (event.timestamp) {
      this.lastEventTimestamp = event.timestamp;
    }

    const eventType = type || event.event_type || '';

    switch (eventType) {
      case 'goal_state_changed':
        this.handleGoalStateChanged(event);
        break;
      case 'draft_ready':
        this.handleDraftReady(event);
        break;
      case 'draft_approved':
        this.handleDraftApproved(event);
        break;
      case 'draft_denied':
        this.handleDraftDenied(event);
        break;
      case 'goal_failed':
        this.handleGoalFailed(event);
        break;
      default:
        break;
    }

    // Refresh panels on any state change
    if (INTERESTING_TYPES.includes(eventType)) {
      this.goalsProvider.refresh();
      this.draftsProvider.refresh();
    }
  }

  private handleGoalStateChanged(event: TaEvent): void {
    const state = event.payload?.state ?? '';
    const title = event.payload?.title ?? 'Goal';

    if (state === 'pr_ready') {
      vscode.window
        .showInformationMessage(
          `Draft ready for review: ${title}`,
          'Review Draft',
        )
        .then((action) => {
          if (action === 'Review Draft') {
            vscode.commands.executeCommand('taDrafts.focus');
            vscode.commands.executeCommand('ta.listDrafts');
          }
        });
      return;
    }

    if (state === 'applied') {
      vscode.window.showInformationMessage(`Goal applied: ${title}`);
      return;
    }

    if (state === 'failed' || state === 'error') {
      vscode.window
        .showWarningMessage(
          `Goal failed: ${title}`,
          'View Shell',
        )
        .then((action) => {
          if (action === 'View Shell') {
            vscode.commands.executeCommand('ta.openShell');
          }
        });
    }
  }

  private handleDraftReady(event: TaEvent): void {
    const title = event.payload?.draft_title ?? event.payload?.title ?? 'New draft';
    vscode.window
      .showInformationMessage(
        `Draft ready: ${title}`,
        'Review',
        'Approve',
      )
      .then((action) => {
        if (action === 'Review') {
          vscode.commands.executeCommand('taDrafts.focus');
        } else if (action === 'Approve') {
          vscode.commands.executeCommand('ta.approveDraft');
        }
      });
  }

  private handleDraftApproved(event: TaEvent): void {
    const title = event.payload?.draft_title ?? event.payload?.title ?? 'Draft';
    vscode.window.showInformationMessage(`Draft approved and applied: ${title}`);
  }

  private handleDraftDenied(event: TaEvent): void {
    const title = event.payload?.draft_title ?? event.payload?.title ?? 'Draft';
    vscode.window.showWarningMessage(`Draft denied: ${title}`);
  }

  private handleGoalFailed(event: TaEvent): void {
    const title = event.payload?.title ?? 'Goal';
    const message = event.payload?.message ?? 'No details available';
    vscode.window
      .showWarningMessage(
        `Goal failed: ${title}. ${message}`,
        'Open Shell',
      )
      .then((action) => {
        if (action === 'Open Shell') {
          vscode.commands.executeCommand('ta.openShell');
        }
      });
  }

  private scheduleReconnect(delayMs: number): void {
    if (this.stopped) {
      return;
    }
    this.reconnectTimer = setTimeout(() => {
      if (!this.stopped) {
        this.connect();
      }
    }, delayMs);
  }
}

// Re-export for convenience
export { GOAL_COMPLETE_STATES };
