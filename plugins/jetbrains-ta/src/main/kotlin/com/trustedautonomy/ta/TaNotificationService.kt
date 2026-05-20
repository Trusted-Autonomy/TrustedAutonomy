package com.trustedautonomy.ta

import com.google.gson.Gson
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.components.Service
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import java.io.Closeable

private data class TaEvent(
    val event_type: String? = null,
    val timestamp: String? = null,
    val payload: Map<String, Any>? = null,
)

private val INTERESTING_TYPES = setOf(
    "goal_state_changed",
    "draft_ready",
    "draft_approved",
    "draft_denied",
    "goal_failed",
)

@Service(Service.Level.PROJECT)
class TaNotificationService(private val project: Project) : Disposable {

    private val gson = Gson()
    private var sseStream: Closeable? = null
    private var lastEventTimestamp: String? = null

    @Volatile
    private var running = false

    fun start() {
        if (running) return
        running = true
        connect()
    }

    private fun connect() {
        if (!running) return
        val client = TaSettings.getInstance().newClient()
        sseStream = client.openEventStream(
            onEvent = { type, data -> onEvent(type, data) },
            onError = { err ->
                if (running) {
                    Thread.sleep(15_000)
                    connect()
                }
            },
            since = lastEventTimestamp,
            types = INTERESTING_TYPES.joinToString(","),
        )
    }

    private fun onEvent(type: String, data: String) {
        val event = try {
            gson.fromJson(data, TaEvent::class.java)
        } catch (_: Exception) {
            return
        }

        event.timestamp?.let { lastEventTimestamp = it }
        val eventType = type.ifBlank { event.event_type ?: "" }
        val payload = event.payload ?: emptyMap()

        when (eventType) {
            "goal_state_changed" -> handleGoalStateChanged(payload)
            "draft_ready" -> handleDraftReady(payload)
            "draft_approved" -> handleDraftApproved(payload)
            "draft_denied" -> handleDraftDenied(payload)
            "goal_failed" -> handleGoalFailed(payload)
        }
    }

    private fun handleGoalStateChanged(payload: Map<String, Any>) {
        val state = payload["state"] as? String ?: return
        val title = payload["title"] as? String ?: "Goal"
        when (state) {
            "pr_ready" -> notify("Draft ready for review: $title", NotificationType.INFORMATION)
            "applied" -> notify("Goal applied: $title", NotificationType.INFORMATION)
            "failed", "error" -> notify("Goal failed: $title — check `ta status`", NotificationType.WARNING)
        }
    }

    private fun handleDraftReady(payload: Map<String, Any>) {
        val title = (payload["draft_title"] ?: payload["title"]) as? String ?: "New draft"
        notify("Draft ready: $title", NotificationType.INFORMATION)
    }

    private fun handleDraftApproved(payload: Map<String, Any>) {
        val title = (payload["draft_title"] ?: payload["title"]) as? String ?: "Draft"
        notify("Draft approved and applied: $title", NotificationType.INFORMATION)
    }

    private fun handleDraftDenied(payload: Map<String, Any>) {
        val title = (payload["draft_title"] ?: payload["title"]) as? String ?: "Draft"
        notify("Draft denied: $title", NotificationType.WARNING)
    }

    private fun handleGoalFailed(payload: Map<String, Any>) {
        val title = payload["title"] as? String ?: "Goal"
        val message = payload["message"] as? String ?: "No details available"
        notify("Goal failed: $title. $message", NotificationType.ERROR)
    }

    private fun notify(content: String, type: NotificationType) {
        val group = NotificationGroupManager.getInstance()
            .getNotificationGroup("Trusted Autonomy") ?: return
        group.createNotification(content, type).notify(project)
    }

    override fun dispose() {
        running = false
        sseStream?.close()
    }

    companion object {
        @JvmStatic
        fun getInstance(project: Project): TaNotificationService =
            project.getService(TaNotificationService::class.java)
    }
}
