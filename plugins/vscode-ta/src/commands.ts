import * as vscode from 'vscode';
import { TaClient, DraftPackage } from './client';
import { GoalsProvider } from './goalsProvider';
import { DraftsProvider, DraftItem } from './draftsProvider';
import { DraftDiffProvider, openDraftDiff, pathFromUri } from './diffProvider';

export function registerCommands(
  context: vscode.ExtensionContext,
  client: TaClient,
  goalsProvider: GoalsProvider,
  draftsProvider: DraftsProvider,
  diffProvider: DraftDiffProvider,
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('ta.startGoal', () => startGoal(client, goalsProvider, draftsProvider)),
    vscode.commands.registerCommand('ta.listDrafts', () => draftsProvider.refresh()),
    vscode.commands.registerCommand('ta.refreshGoals', () => goalsProvider.refresh()),
    vscode.commands.registerCommand('ta.refreshDrafts', () => draftsProvider.refresh()),
    vscode.commands.registerCommand('ta.approveDraft', (item?: DraftItem) => approveDraft(client, draftsProvider, item)),
    vscode.commands.registerCommand('ta.denyDraft', (item?: DraftItem) => denyDraft(client, draftsProvider, item)),
    vscode.commands.registerCommand('ta.viewDiff', (item?: DraftItem) => viewDiff(client, diffProvider, item)),
    vscode.commands.registerCommand('ta.openShell', () => openShell()),
  );
}

async function startGoal(
  client: TaClient,
  goalsProvider: GoalsProvider,
  draftsProvider: DraftsProvider,
): Promise<void> {
  const title = await vscode.window.showInputBox({
    title: 'Start New TA Goal',
    prompt: 'Describe what you want the agent to accomplish',
    placeHolder: 'e.g. Add input validation to the login form',
    validateInput: (v) => (v.trim().length < 3 ? 'Please enter a goal description (at least 3 characters)' : undefined),
  });

  if (!title) {
    return;
  }

  const phase = await vscode.window.showInputBox({
    title: 'Plan Phase (optional)',
    prompt: 'Link to a plan phase ID, or leave blank',
    placeHolder: 'e.g. v0.16.0 (optional)',
  });

  const phaseArg = phase?.trim() ? ` --phase "${phase.trim()}"` : '';
  const command = `ta run "${title.replace(/"/g, '\\"')}"${phaseArg}`;

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: `Starting goal: ${title}`,
      cancellable: false,
    },
    async () => {
      try {
        const result = await client.runCommand(command);
        if (result.exit_code === 0) {
          vscode.window.showInformationMessage(
            `Goal started: ${title}`,
            'View Goals',
          ).then((action) => {
            if (action === 'View Goals') {
              vscode.commands.executeCommand('taGoals.focus');
            }
          });
          goalsProvider.refresh();
          draftsProvider.refresh();
        } else {
          const detail = result.stderr || result.stdout || 'No details available';
          vscode.window.showErrorMessage(
            `Failed to start goal: ${detail.slice(0, 120)}`,
            'Show Output',
          ).then((action) => {
            if (action === 'Show Output') {
              showCommandOutput(title, result.stdout, result.stderr);
            }
          });
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(
          `Cannot reach TA daemon: ${msg.slice(0, 120)}. Is it running?`,
          'Configure Settings',
        ).then((action) => {
          if (action === 'Configure Settings') {
            vscode.commands.executeCommand('workbench.action.openSettings', 'ta.daemonUrl');
          }
        });
      }
    },
  );
}

async function approveDraft(
  client: TaClient,
  draftsProvider: DraftsProvider,
  item?: DraftItem,
): Promise<void> {
  const draftId = await resolveDraftId(client, item, 'approve');
  if (!draftId) {
    return;
  }

  const label = item?.draft.title ?? draftId;
  const confirmed = await vscode.window.showWarningMessage(
    `Approve draft "${label}"? This will apply all changes to your project.`,
    { modal: true },
    'Approve',
  );
  if (confirmed !== 'Approve') {
    return;
  }

  try {
    const result = await client.approveDraft(draftId);
    vscode.window.showInformationMessage(`Draft approved: ${result.message}`);
    draftsProvider.refresh();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Approve failed: ${msg.slice(0, 200)}`);
  }
}

async function denyDraft(
  client: TaClient,
  draftsProvider: DraftsProvider,
  item?: DraftItem,
): Promise<void> {
  const draftId = await resolveDraftId(client, item, 'deny');
  if (!draftId) {
    return;
  }

  const reason = await vscode.window.showInputBox({
    title: 'Deny Draft',
    prompt: 'Reason for denial (will be visible to the agent on follow-up)',
    placeHolder: 'e.g. The validation logic is incorrect — see comment inline',
    validateInput: (v) => (v.trim().length < 3 ? 'Please provide a reason (at least 3 characters)' : undefined),
  });

  if (reason === undefined) {
    return;
  }

  try {
    const result = await client.denyDraft(draftId, reason || 'Denied via VS Code extension');
    vscode.window.showInformationMessage(`Draft denied: ${result.message}`);
    draftsProvider.refresh();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Deny failed: ${msg.slice(0, 200)}`);
  }
}

async function viewDiff(
  client: TaClient,
  diffProvider: DraftDiffProvider,
  item?: DraftItem,
): Promise<void> {
  const draftId = await resolveDraftId(client, item, 'view');
  if (!draftId) {
    return;
  }

  let draft: DraftPackage;
  try {
    draft = await client.getDraft(draftId);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Cannot load draft: ${msg.slice(0, 200)}`);
    return;
  }

  const artifacts = draft.changes.artifacts.filter((a) => pathFromUri(a.resource_uri));

  if (artifacts.length === 0) {
    vscode.window.showInformationMessage('This draft has no reviewable file changes.');
    return;
  }

  if (artifacts.length === 1) {
    await openDraftDiff(draft, artifacts[0], diffProvider);
    return;
  }

  // Multiple files: let the user pick
  const picks = artifacts.map((a) => ({
    label: pathFromUri(a.resource_uri) ?? a.resource_uri,
    description: a.change_type,
    detail: a.rationale,
    artifact: a,
  }));

  const selection = await vscode.window.showQuickPick(picks, {
    title: `View diff — ${draft.goal.title} (${artifacts.length} files)`,
    placeHolder: 'Select a file to review',
    canPickMany: false,
  });

  if (selection) {
    await openDraftDiff(draft, selection.artifact, diffProvider);
  }
}

async function openShell(): Promise<void> {
  const url = vscode.workspace.getConfiguration('ta').get<string>('daemonUrl') ?? 'http://127.0.0.1:7700';
  const shellUrl = `${url}/shell`;
  await vscode.env.openExternal(vscode.Uri.parse(shellUrl));
}

async function resolveDraftId(
  client: TaClient,
  item: DraftItem | undefined,
  _action: string,
): Promise<string | undefined> {
  if (item) {
    return item.draft.package_id;
  }

  // Fallback: show a quick pick of available drafts
  let drafts;
  try {
    drafts = await client.listDrafts();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Cannot load drafts: ${msg.slice(0, 200)}`);
    return undefined;
  }

  const active = drafts.filter(
    (d) => !['applied', 'superseded', 'closed'].includes(d.status.toLowerCase()),
  );

  if (active.length === 0) {
    vscode.window.showInformationMessage('No pending drafts to review.');
    return undefined;
  }

  const pick = await vscode.window.showQuickPick(
    active.map((d) => ({
      label: d.title || d.package_id.slice(0, 12),
      description: d.status,
      detail: `${d.artifact_count} file${d.artifact_count !== 1 ? 's' : ''} · ${d.created_at}`,
      id: d.package_id,
    })),
    { title: 'Select Draft', placeHolder: 'Choose a draft to act on' },
  );

  return pick?.id;
}

function showCommandOutput(title: string, stdout: string, stderr: string): void {
  const channel = vscode.window.createOutputChannel(`TA: ${title}`);
  channel.appendLine(`=== stdout ===`);
  channel.appendLine(stdout || '(empty)');
  if (stderr) {
    channel.appendLine(`\n=== stderr ===`);
    channel.appendLine(stderr);
  }
  channel.show();
}
