import * as vscode from 'vscode';
import { TaClient, DraftSummary } from './client';

const STATUS_ICONS: Record<string, vscode.ThemeIcon> = {
  draft: new vscode.ThemeIcon('circle-large-outline', new vscode.ThemeColor('charts.gray')),
  pendingreview: new vscode.ThemeIcon('git-pull-request', new vscode.ThemeColor('charts.yellow')),
  pending_review: new vscode.ThemeIcon('git-pull-request', new vscode.ThemeColor('charts.yellow')),
  approved: new vscode.ThemeIcon('check', new vscode.ThemeColor('charts.green')),
  denied: new vscode.ThemeIcon('x', new vscode.ThemeColor('charts.red')),
  applied: new vscode.ThemeIcon('pass', new vscode.ThemeColor('charts.green')),
  superseded: new vscode.ThemeIcon('archive', new vscode.ThemeColor('charts.gray')),
  closed: new vscode.ThemeIcon('archive', new vscode.ThemeColor('charts.gray')),
};

function statusIcon(status: string): vscode.ThemeIcon {
  const key = status.toLowerCase().replace(/[_\s-]+/g, '');
  return STATUS_ICONS[key] ?? new vscode.ThemeIcon('circle-outline');
}

function formatDate(iso: string): string {
  try {
    const d = new Date(iso);
    const now = new Date();
    const diffMs = now.getTime() - d.getTime();
    const diffMins = Math.floor(diffMs / 60_000);
    if (diffMins < 1) {
      return 'just now';
    }
    if (diffMins < 60) {
      return `${diffMins}m ago`;
    }
    if (diffMins < 1440) {
      return `${Math.floor(diffMins / 60)}h ago`;
    }
    return d.toLocaleDateString();
  } catch {
    return iso;
  }
}

export class DraftItem extends vscode.TreeItem {
  constructor(public readonly draft: DraftSummary) {
    super(draft.title || draft.package_id.slice(0, 8), vscode.TreeItemCollapsibleState.None);

    this.id = draft.package_id;
    this.iconPath = statusIcon(draft.status);
    this.description = `${draft.status} · ${draft.artifact_count} file${draft.artifact_count !== 1 ? 's' : ''} · ${formatDate(draft.created_at)}`;
    this.tooltip = new vscode.MarkdownString(
      `**${draft.title}**\n\n` +
      `Status: \`${draft.status}\`\n\n` +
      `Files changed: ${draft.artifact_count}\n\n` +
      `Created: ${new Date(draft.created_at).toLocaleString()}\n\n` +
      `ID: \`${draft.package_id}\``
    );
    this.contextValue = 'draft';
  }
}

class EmptyItem extends vscode.TreeItem {
  constructor(label: string) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon('info');
    this.contextValue = 'empty';
  }
}

class ErrorItem extends vscode.TreeItem {
  constructor(message: string) {
    super(message, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon('circle-slash', new vscode.ThemeColor('errorForeground'));
    this.contextValue = 'error';
  }
}

export class DraftsProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChangeTreeData = new vscode.EventEmitter<vscode.TreeItem | undefined | void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private items: vscode.TreeItem[] = [new EmptyItem('Loading…')];
  private timer?: NodeJS.Timeout;

  constructor(private readonly client: TaClient) {
    this.startPolling();
  }

  refresh(): void {
    this.load();
  }

  private startPolling(): void {
    this.load();
    const intervalSecs = vscode.workspace.getConfiguration('ta').get<number>('pollIntervalSeconds') ?? 10;
    this.timer = setInterval(() => this.load(), intervalSecs * 1000);
  }

  stopPolling(): void {
    if (this.timer) {
      clearInterval(this.timer);
    }
  }

  private async load(): Promise<void> {
    try {
      const drafts = await this.client.listDrafts();
      const active = drafts.filter(
        (d) => !['applied', 'superseded', 'closed', 'denied'].includes(d.status.toLowerCase()),
      );
      if (active.length === 0) {
        this.items = [new EmptyItem('No pending drafts')];
      } else {
        this.items = active.map((d) => new DraftItem(d));
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      this.items = [new ErrorItem(`Cannot load drafts: ${msg.slice(0, 60)}`)];
    }
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: vscode.TreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(_element?: vscode.TreeItem): vscode.TreeItem[] {
    return this.items;
  }
}
