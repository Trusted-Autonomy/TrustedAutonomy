import * as vscode from 'vscode';
import { TaClient } from './client';
import { GoalsProvider } from './goalsProvider';
import { DraftsProvider } from './draftsProvider';
import { DraftDiffProvider, DIFF_SCHEME } from './diffProvider';
import { registerCommands } from './commands';
import { NotificationListener } from './notifications';

export function activate(context: vscode.ExtensionContext): void {
  const client = new TaClient();
  const goalsProvider = new GoalsProvider(client);
  const draftsProvider = new DraftsProvider(client);
  const diffProvider = new DraftDiffProvider(client);

  // Register tree views
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('taGoals', goalsProvider),
    vscode.window.registerTreeDataProvider('taDrafts', draftsProvider),
  );

  // Register virtual document provider for diff views
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(DIFF_SCHEME, diffProvider),
  );

  // Register all commands
  registerCommands(context, client, goalsProvider, draftsProvider, diffProvider);

  // Status bar item showing daemon health and active goal count
  const statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  statusBar.command = 'ta.openShell';
  statusBar.show();
  context.subscriptions.push(statusBar);

  // SSE notification listener for real-time goal/draft events
  const notifications = new NotificationListener(client, goalsProvider, draftsProvider);
  notifications.start();
  context.subscriptions.push({
    dispose: () => {
      notifications.stop();
      goalsProvider.stopPolling();
      draftsProvider.stopPolling();
    },
  });

  // Poll daemon health for the status bar
  const updateStatusBar = async (): Promise<void> => {
    try {
      const health = await client.health();
      if (health.status === 'ok') {
        try {
          const status = await client.getStatus();
          const active = status.active_goals ?? 0;
          statusBar.text = active > 0 ? `$(sync~spin) TA: ${active} running` : `$(check) TA: ready`;
          statusBar.tooltip = new vscode.MarkdownString(
            `**Trusted Autonomy** v${health.version}\n\n` +
            `${active} active goal${active !== 1 ? 's' : ''}\n\n` +
            `${status.pending_drafts} pending draft${status.pending_drafts !== 1 ? 's' : ''}\n\n` +
            `Click to open shell`,
          );
          statusBar.backgroundColor = undefined;
        } catch {
          statusBar.text = `$(check) TA: v${health.version}`;
          statusBar.backgroundColor = undefined;
        }
      } else {
        statusBar.text = '$(warning) TA: error';
        statusBar.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
        statusBar.tooltip = 'TA daemon returned an error response';
      }
    } catch {
      statusBar.text = '$(circle-slash) TA: offline';
      statusBar.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
      statusBar.tooltip = new vscode.MarkdownString(
        '**TA daemon is not running**\n\nStart it with `ta start` in your project directory.\n\nClick to open settings.',
      );
      statusBar.command = 'workbench.action.openSettings';
    }
  };

  updateStatusBar();
  const statusPollTimer = setInterval(updateStatusBar, 15_000);
  context.subscriptions.push({ dispose: () => clearInterval(statusPollTimer) });

  // Show a one-time welcome message when the extension first activates
  const hasShownWelcome = context.globalState.get<boolean>('ta.welcomeShown');
  if (!hasShownWelcome) {
    context.globalState.update('ta.welcomeShown', true);
    vscode.window
      .showInformationMessage(
        'Trusted Autonomy extension activated. Start a goal from the Command Palette.',
        'Start Goal',
        'Get Started',
      )
      .then((action) => {
        if (action === 'Start Goal') {
          vscode.commands.executeCommand('ta.startGoal');
        } else if (action === 'Get Started') {
          vscode.commands.executeCommand(
            'workbench.action.openWalkthrough',
            'trusted-autonomy.ta#ta.getStarted',
          );
        }
      });
  }
}

export function deactivate(): void {
  // Cleanup handled via context.subscriptions
}
