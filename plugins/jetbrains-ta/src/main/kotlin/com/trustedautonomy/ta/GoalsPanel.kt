package com.trustedautonomy.ta

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import java.awt.BorderLayout
import java.awt.Component
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

private data class GoalEntry(val label: String, val agent: AgentInfo?)

class GoalsPanel(private val project: Project) : JPanel(BorderLayout()) {

    private val model = DefaultListModel<GoalEntry>()
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
                val entry = value as? GoalEntry
                val label = entry?.label ?: ""
                val comp = super.getListCellRendererComponent(list, label, index, isSelected, cellHasFocus) as JLabel
                comp.toolTipText = entry?.agent?.let {
                    "Goal: ${it.goal_id}\nState: ${it.state}\nRunning: ${formatDuration(it.running_secs)}"
                }
                return comp
            }
        }

        val toolbar = JPanel()
        val refreshBtn = JButton("Refresh")
        refreshBtn.addActionListener { refresh() }
        toolbar.add(refreshBtn)

        add(toolbar, BorderLayout.NORTH)
        add(JBScrollPane(list), BorderLayout.CENTER)

        startPolling()
    }

    private fun startPolling() {
        val intervalSecs = TaSettings.getInstance().state.pollIntervalSeconds.toLong()
        val executor = ScheduledThreadPoolExecutor(1) { r ->
            Thread(r, "ta-goals-poll").also { it.isDaemon = true }
        }
        pollFuture = executor.scheduleWithFixedDelay({ refresh() }, 0, intervalSecs, TimeUnit.SECONDS)
    }

    fun refresh() {
        ApplicationManager.getApplication().executeOnPooledThread {
            val entries = try {
                val client = TaSettings.getInstance().newClient()
                val status = client.getStatus()
                if (status.active_agents.isEmpty()) {
                    listOf(GoalEntry("No active goals", null))
                } else {
                    status.active_agents.map { agent ->
                        val duration = formatDuration(agent.running_secs)
                        GoalEntry("${agent.title.ifBlank { agent.tag.ifBlank { agent.goal_id.take(8) } }} [${agent.state}] · $duration", agent)
                    }
                }
            } catch (e: Exception) {
                listOf(GoalEntry("Daemon offline — ${e.message?.take(60) ?: "unknown error"}", null))
            }
            SwingUtilities.invokeLater {
                model.clear()
                entries.forEach { model.addElement(it) }
            }
        }
    }

    fun dispose() {
        pollFuture?.cancel(true)
    }

    private fun formatDuration(secs: Long): String = when {
        secs < 60 -> "${secs}s"
        secs < 3600 -> "${secs / 60}m ${secs % 60}s"
        else -> "${secs / 3600}h ${(secs % 3600) / 60}m"
    }
}
