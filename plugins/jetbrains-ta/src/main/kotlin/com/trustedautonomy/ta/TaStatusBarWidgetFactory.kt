package com.trustedautonomy.ta

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

class TaStatusBarWidgetFactory : StatusBarWidgetFactory {
    override fun getId(): String = TaStatusBarWidget.ID
    override fun getDisplayName(): String = "Trusted Autonomy"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = TaStatusBarWidget(project)
    override fun disposeWidget(widget: StatusBarWidget) = widget.dispose()
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}

class TaStatusBarWidget(private val project: Project) :
    StatusBarWidget, StatusBarWidget.TextPresentation {

    companion object {
        const val ID = "com.trusted-autonomy.ta.statusBar"
        private const val POLL_INTERVAL_SECS = 15L
    }

    private var statusBar: StatusBar? = null
    private var text = "TA: connecting…"
    private var tooltip = "Trusted Autonomy — checking daemon status"
    private var pollFuture: ScheduledFuture<*>? = null

    override fun ID(): String = ID
    override fun getPresentation(): StatusBarWidget.WidgetPresentation = this
    override fun getText(): String = text
    override fun getTooltipText(): String = tooltip
    override fun getAlignment(): Float = 0.5f

    override fun install(statusBar: StatusBar) {
        this.statusBar = statusBar
        pollFuture = ApplicationManager.getApplication().executeOnPooledThread {
            schedulePoll()
        }.let { null } // schedulePoll runs its own loop
        schedulePoll()
    }

    private fun schedulePoll() {
        val executor = java.util.concurrent.Executors.newSingleThreadScheduledExecutor { r ->
            Thread(r, "ta-statusbar-poll").also { it.isDaemon = true }
        }
        pollFuture = executor.scheduleWithFixedDelay(
            { update() },
            0,
            POLL_INTERVAL_SECS,
            TimeUnit.SECONDS,
        )
    }

    fun update() {
        try {
            val client = TaSettings.getInstance().newClient()
            val health = client.health()
            val status = try {
                client.getStatus()
            } catch (_: Exception) {
                null
            }
            val active = status?.active_goals ?: 0
            text = if (active > 0) "TA: $active running" else "TA: ready"
            tooltip = buildString {
                append("Trusted Autonomy v${health.version}\n")
                append("$active active goal${if (active != 1) "s" else ""}\n")
                status?.let { append("${it.pending_drafts} pending draft${if (it.pending_drafts != 1) "s" else ""}") }
            }
        } catch (_: Exception) {
            text = "TA: offline"
            tooltip = "TA daemon is not running — start with `ta start`"
        }
        statusBar?.updateWidget(ID)
    }

    override fun dispose() {
        pollFuture?.cancel(true)
        statusBar = null
    }
}
