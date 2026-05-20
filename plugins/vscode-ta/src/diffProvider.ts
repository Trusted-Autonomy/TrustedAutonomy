import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { TaClient, DraftPackage, Artifact } from './client';

const SCHEME = 'ta-draft';

/**
 * Virtual document provider for TA draft artifact content.
 *
 * URI format: ta-draft://<draft-id>/<relative-file-path>?side=original|staged
 *
 * For the "staged" side, the content comes from the staging directory:
 *   <project_root>/.ta/staging/<goal_id>/<path>
 *
 * For the "original" side, the content comes from the project root:
 *   <project_root>/<path>
 *
 * When staging is not available (goal already applied), falls back to
 * showing the diff text as-is in the document content.
 */
export class DraftDiffProvider implements vscode.TextDocumentContentProvider {
  private draftCache = new Map<string, DraftPackage>();

  constructor(private readonly client: TaClient) {}

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const draftId = uri.authority;
    const relativePath = uri.path.slice(1); // remove leading /
    const side = new URLSearchParams(uri.query).get('side') ?? 'staged';

    let draft = this.draftCache.get(draftId);
    if (!draft) {
      try {
        draft = await this.client.getDraft(draftId);
        this.draftCache.set(draftId, draft);
      } catch (err) {
        return `// Could not load draft ${draftId}: ${err}`;
      }
    }

    const projectRoot = await resolveProjectRoot();

    if (side === 'original') {
      const originalPath = path.join(projectRoot, relativePath);
      try {
        return fs.readFileSync(originalPath, 'utf8');
      } catch {
        return '';
      }
    }

    // staged side: try staging directory first
    const stagingPath = path.join(
      projectRoot,
      '.ta',
      'staging',
      draft.goal.goal_id,
      relativePath,
    );
    try {
      return fs.readFileSync(stagingPath, 'utf8');
    } catch {
      // staging not available — return empty string (file may be deleted or goal applied)
      return `// Staged content not available for ${relativePath}\n// Apply staging may have already been completed.`;
    }
  }

  invalidate(draftId: string): void {
    this.draftCache.delete(draftId);
  }

  clearCache(): void {
    this.draftCache.clear();
  }
}

async function resolveProjectRoot(): Promise<string> {
  const folders = vscode.workspace.workspaceFolders;
  if (folders && folders.length > 0) {
    return folders[0].uri.fsPath;
  }
  return process.cwd();
}

export function pathFromUri(resourceUri: string): string | undefined {
  const prefix = 'fs://workspace/';
  if (resourceUri.startsWith(prefix)) {
    return resourceUri.slice(prefix.length);
  }
  return undefined;
}

export async function openDraftDiff(
  draft: DraftPackage,
  artifact: Artifact,
  provider: DraftDiffProvider,
): Promise<void> {
  const relativePath = pathFromUri(artifact.resource_uri);
  if (!relativePath) {
    vscode.window.showWarningMessage(
      `Cannot open diff for ${artifact.resource_uri} — only fs:// URIs are supported.`,
    );
    return;
  }

  const changeType = artifact.change_type?.toLowerCase() ?? '';

  if (changeType === 'delete' || changeType === 'remove' || changeType === 'deleted') {
    // Show deleted file: original vs empty
    const originalUri = makeUri(draft.package_id, relativePath, 'original');
    const emptyUri = makeUri(draft.package_id, relativePath, 'staged');
    await vscode.commands.executeCommand(
      'vscode.diff',
      originalUri,
      emptyUri,
      `Deleted: ${path.basename(relativePath)}`,
    );
    return;
  }

  if (changeType === 'add' || changeType === 'create' || changeType === 'added') {
    // Show new file: empty vs staged
    const emptyUri = makeUri(draft.package_id, relativePath, 'original');
    const stagedUri = makeUri(draft.package_id, relativePath, 'staged');
    await vscode.commands.executeCommand(
      'vscode.diff',
      emptyUri,
      stagedUri,
      `Added: ${path.basename(relativePath)}`,
    );
    return;
  }

  // Default: show original vs staged
  const originalUri = makeUri(draft.package_id, relativePath, 'original');
  const stagedUri = makeUri(draft.package_id, relativePath, 'staged');
  await vscode.commands.executeCommand(
    'vscode.diff',
    originalUri,
    stagedUri,
    `Draft: ${path.basename(relativePath)} (${draft.goal.title})`,
  );
}

function makeUri(draftId: string, relativePath: string, side: 'original' | 'staged'): vscode.Uri {
  return vscode.Uri.from({
    scheme: SCHEME,
    authority: draftId,
    path: `/${relativePath}`,
    query: `side=${side}`,
  });
}

export const DIFF_SCHEME = SCHEME;
