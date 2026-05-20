import * as vscode from 'vscode';
import { TaClient, AgentInfo } from './client';

const STATE_ICONS: Record<string, vscode.ThemeIcon> = {
  running: new vscode.ThemeIcon('sync~spin', new vscode.ThemeColor('charts.blue')),
  pr_ready: new vscode.ThemeIcon('git-pull-request', new vscode.ThemeColor('charts.yellow')),
  under_review: new vscode.ThemeIcon('eye', new vscode.ThemeColor('charts.orange')),
  approved: new vscode.ThemeIcon('check', new vscode.ThemeColor('charts.green')),
  applied: new vscode.ThemeIcon('pass', new vscode.ThemeColor('charts.green')),
  failed: new vscode.ThemeIcon('error', new vscode.ThemeColor('charts.red')),
  denied: new vscode.ThemeIcon('x', new vscode.ThemeColor('charts.red')),
};

function stateIcon(state: string): vscode.ThemeIcon {
  const normalized = state.toLowerCase().replace(/[\s-]+/g, '_');
  return STATE_ICONS[normalized] ?? new vscode.ThemeIcon('circle-outline');
}

function formatDuration(secs: number): string {
  if (secs < 60) {
    return `${secs}s`;
  }
  if (secs < 3600) {
    return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  }
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export class GoalItem extends vscode.TreeItem {
  constructor(public readonly agent: AgentInfo) {
    super(agent.title || agent.tag || agent.goal_id.slice(0, 8), vscode.TreeItemCollapsibleState.None);

    this.id = agent.goal_id;
    this.iconPath = stateIcon(agent.state);
    this.tooltip = new vscode.MarkdownString(
      `**${agent.title}**\n\n` +
      `State: \`${agent.state}\`\n\n` +
      `Running: ${formatDuration(agent.running_secs)}\n\n` +
      (agent.vcs_state ? `VCS: ${agent.vcs_state}\n\n` : '') +
      `Goal ID: \`${agent.goal_id}\``
    );
    this.description = `${agent.state} · ${formatDuration(agent.running_secs)}`;
    this.contextValue = 'goal';
  }
}

class DaemonOfflineItem extends vscode.TreeItem {
  constructor(message: string) {
    super(message, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon('circle-slash', new vscode.ThemeColor('errorForeground'));
    this.contextValue = 'offline';
  }
}

class EmptyItem extends vscode.TreeItem {
  constructor(label: string) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon('info');
    this.contextValue = 'empty';
  }
}

export class GoalsProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
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
      const status = await this.client.getStatus();
      if (status.active_agents.length === 0) {
        this.items = [new EmptyItem('No active goals')];
      } else {
        this.items = status.active_agents.map((a) => new GoalItem(a));
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      this.items = [new DaemonOfflineItem(`TA daemon offline: ${msg.slice(0, 60)}`)];
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
