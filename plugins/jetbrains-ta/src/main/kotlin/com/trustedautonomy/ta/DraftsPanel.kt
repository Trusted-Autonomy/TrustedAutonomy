package com.trustedautonomy.ta

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import java.awt.BorderLayout
import java.awt.Component
import java.awt.FlowLayout
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import javax.swing.DefaultListCellRenderer
import javax.swing.DefaultListModel
import javax.swing.JButton
import javax.swing.JLabel
import javax.swing.JList
import javax.swing.JPanel
import javax.swing.SwingUtilities

private val TERMINAL_STATUSES = setOf("applied", "superseded", "closed", "denied")

private data class DraftEntry(val label: String, val draft: DraftSummary?)

class DraftsPanel(private val project: Project) : JPanel(BorderLayout()) {

    private val model = DefaultListModel<DraftEntry>()
    private val list = JBList(model)
    private var pollFuture: ScheduledFuture<*>? = null

    init {
        list.cellRenderer = object : DefaultListCellRenderer() {
            override fun getListCellRendererComponent(
                list: JList<*>?,
                value: Any?,
                index: Int,
                isSelected: Boolean,
                cellHasFocus: Boolean,
            ): Component {
                val entry = value as? DraftEntry
                val label = entry?.label ?: ""
                val comp = super.getListCellRendererComponent(list, label, index, isSelected, cellHasFocus) as JLabel
                comp.toolTipText = entry?.draft?.let {
                    "ID: ${it.package_id}\nStatus: ${it.status}\nFiles: ${it.artifact_count}\nCreated: ${it.created_at}"
                }
                return comp
            }
        }

        val toolbar = JPanel(FlowLayout(FlowLayout.LEFT))
        val refreshBtn = JButton("Refresh")
        val approveBtn = JButton("Approve")
        val denyBtn = JButton("Deny")

        refreshBtn.addActionListener { refresh() }
        approveBtn.addActionListener { approveSelected() }
        denyBtn.addActionListener { denySelected() }

        toolbar.add(refreshBtn)
        toolbar.add(approveBtn)
        toolbar.add(denyBtn)

        add(toolbar, BorderLayout.NORTH)
        add(JBScrollPane(list), BorderLayout.CENTER)

        startPolling()
    }

    private fun startPolling() {
        val intervalSecs = TaSettings.getInstance().state.pollIntervalSeconds.toLong()
        val executor = ScheduledThreadPoolExecutor(1) { r ->
            Thread(r, "ta-drafts-poll").also { it.isDaemon = true }
        }
        pollFuture = executor.scheduleWithFixedDelay({ refresh() }, 0, intervalSecs, TimeUnit.SECONDS)
    }

    fun refresh() {
        ApplicationManager.getApplication().executeOnPooledThread {
            val entries = try {
                val client = TaSettings.getInstance().newClient()
                val active = client.listDrafts()
                    .filterNot { TERMINAL_STATUSES.contains(it.status.lowercase()) }
                if (active.isEmpty()) {
                    listOf(DraftEntry("No pending drafts", null))
                } else {
                    active.map { d ->
                        val files = "${d.artifact_count} file${if (d.artifact_count != 1) "s" else ""}"
                        DraftEntry("${d.title.ifBlank { d.package_id.take(8) }} [${d.status}] · $files", d)
                    }
                }
            } catch (e: Exception) {
                listOf(DraftEntry("Cannot load drafts — ${e.message?.take(60) ?: "unknown error"}", null))
            }
            SwingUtilities.invokeLater {
                model.clear()
                entries.forEach { model.addElement(it) }
            }
        }
    }

    private fun selectedDraft(): DraftSummary? {
        val entry = list.selectedValue as? DraftEntry ?: run {
            Messages.showInfoMessage(project, "Select a draft from the list first.", "Trusted Autonomy")
            return null
        }
        return entry.draft ?: run {
            Messages.showInfoMessage(project, "No pending drafts to act on.", "Trusted Autonomy")
            null
        }
    }

    private fun approveSelected() {
        val draft = selectedDraft() ?: return
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
                SwingUtilities.invokeLater {
                    Messages.showInfoMessage(project, result.message.ifBlank { "Draft approved." }, "Trusted Autonomy")
                    refresh()
                }
            } catch (e: Exception) {
                SwingUtilities.invokeLater {
                    Messages.showErrorDialog(project, "Approve failed: ${e.message}", "Trusted Autonomy")
                }
            }
        }
    }

    private fun denySelected() {
        val draft = selectedDraft() ?: return
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
                SwingUtilities.invokeLater {
                    Messages.showInfoMessage(project, result.message.ifBlank { "Draft denied." }, "Trusted Autonomy")
                    refresh()
                }
            } catch (e: Exception) {
                SwingUtilities.invokeLater {
                    Messages.showErrorDialog(project, "Deny failed: ${e.message}", "Trusted Autonomy")
                }
            }
        }
    }

    fun dispose() {
        pollFuture?.cancel(true)
    }
}
