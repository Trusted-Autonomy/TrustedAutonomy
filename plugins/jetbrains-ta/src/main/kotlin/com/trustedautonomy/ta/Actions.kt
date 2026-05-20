package com.trustedautonomy.ta

import com.intellij.ide.BrowserUtil
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.ui.Messages

class StartGoalAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return

        val title = Messages.showInputDialog(
            project,
            "Describe what you want the agent to accomplish:",
            "Start New TA Goal",
            Messages.getQuestionIcon(),
            "",
            null,
        ) ?: return

        if (title.isBlank()) {
            Messages.showWarningDialog(project, "Goal description cannot be empty.", "Trusted Autonomy")
            return
        }

        val phase = Messages.showInputDialog(
            project,
            "Plan phase (optional — leave blank to skip):",
            "Start New TA Goal",
            null,
            "",
            null,
        )

        val phaseArg = if (!phase.isNullOrBlank()) " --phase \"${phase.trim()}\"" else ""
        val command = "ta run \"${title.replace("\"", "\\\"")}\"$phaseArg"

        ProgressManager.getInstance().run(object : Task.Backgroundable(project, "Starting goal: $title", false) {
            override fun run(indicator: ProgressIndicator) {
                indicator.text = "Sending command to TA daemon…"
                try {
                    val result = TaSettings.getInstance().newClient().runCommand(command)
                    ApplicationManager.getApplication().invokeLater {
                        if (result.exit_code == 0) {
                            Messages.showInfoMessage(project, "Goal started: $title", "Trusted Autonomy")
                        } else {
                            val detail = result.stderr.ifBlank { result.stdout }.take(200).ifBlank { "No details." }
                            Messages.showErrorDialog(project, "Failed to start goal:\n$detail", "Trusted Autonomy")
                        }
                    }
                } catch (ex: Exception) {
                    ApplicationManager.getApplication().invokeLater {
                        Messages.showErrorDialog(
                            project,
                            "Cannot reach TA daemon: ${ex.message}\n\nIs it running? Try: ta start",
                            "Trusted Autonomy",
                        )
                    }
                }
            }
        })
    }
}

class ApproveDraftAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return

        val drafts = try {
            TaSettings.getInstance().newClient().listDrafts()
                .filterNot { listOf("applied", "superseded", "closed", "denied").contains(it.status.lowercase()) }
        } catch (ex: Exception) {
            Messages.showErrorDialog(project, "Cannot load drafts: ${ex.message}", "Trusted Autonomy")
            return
        }

        if (drafts.isEmpty()) {
            Messages.showInfoMessage(project, "No pending drafts to approve.", "Trusted Autonomy")
            return
        }

        val labels = drafts.map { "${it.title.ifBlank { it.package_id.take(12) }} [${it.status}]" }.toTypedArray()
        val choice = Messages.showChooseDialog(
            project,
            "Select a draft to approve:",
            "Approve Draft",
            Messages.getQuestionIcon(),
            labels,
            labels[0],
        )
        if (choice < 0) return

        val draft = drafts[choice]
        val confirmed = Messages.showYesNoDialog(
            project,
            "Approve draft \"${draft.title.ifBlank { draft.package_id.take(12) }}\"?\nThis will apply all changes to your project.",
            "Approve Draft",
            "Approve",
            "Cancel",
            Messages.getQuestionIcon(),
        )
        if (confirmed != Messages.YES) return

        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                val result = TaSettings.getInstance().newClient().approveDraft(draft.package_id)
                ApplicationManager.getApplication().invokeLater {
                    Messages.showInfoMessage(project, result.message.ifBlank { "Draft approved." }, "Trusted Autonomy")
                }
            } catch (ex: Exception) {
                ApplicationManager.getApplication().invokeLater {
                    Messages.showErrorDialog(project, "Approve failed: ${ex.message}", "Trusted Autonomy")
                }
            }
        }
    }
}

class DenyDraftAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return

        val drafts = try {
            TaSettings.getInstance().newClient().listDrafts()
                .filterNot { listOf("applied", "superseded", "closed", "denied").contains(it.status.lowercase()) }
        } catch (ex: Exception) {
            Messages.showErrorDialog(project, "Cannot load drafts: ${ex.message}", "Trusted Autonomy")
            return
        }

        if (drafts.isEmpty()) {
            Messages.showInfoMessage(project, "No pending drafts to deny.", "Trusted Autonomy")
            return
        }

        val labels = drafts.map { "${it.title.ifBlank { it.package_id.take(12) }} [${it.status}]" }.toTypedArray()
        val choice = Messages.showChooseDialog(
            project,
            "Select a draft to deny:",
            "Deny Draft",
            Messages.getWarningIcon(),
            labels,
            labels[0],
        )
        if (choice < 0) return

        val draft = drafts[choice]
        val reason = Messages.showInputDialog(
            project,
            "Reason for denial (will be visible to the agent on follow-up):",
            "Deny Draft",
            Messages.getWarningIcon(),
        ) ?: return

        if (reason.isBlank()) {
            Messages.showWarningDialog(project, "Please provide a denial reason.", "Trusted Autonomy")
            return
        }

        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                val result = TaSettings.getInstance().newClient().denyDraft(draft.package_id, reason)
                ApplicationManager.getApplication().invokeLater {
                    Messages.showInfoMessage(project, result.message.ifBlank { "Draft denied." }, "Trusted Autonomy")
                }
            } catch (ex: Exception) {
                ApplicationManager.getApplication().invokeLater {
                    Messages.showErrorDialog(project, "Deny failed: ${ex.message}", "Trusted Autonomy")
                }
            }
        }
    }
}

class OpenShellAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val url = TaSettings.getInstance().state.daemonUrl
        BrowserUtil.browse("$url/shell")
    }
}
