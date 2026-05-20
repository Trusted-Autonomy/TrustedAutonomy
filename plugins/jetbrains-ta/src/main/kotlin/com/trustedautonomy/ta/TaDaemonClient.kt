package com.trustedautonomy.ta

import com.google.gson.Gson
import com.google.gson.reflect.TypeToken
import java.io.Closeable
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration

data class HealthResponse(
    val status: String,
    val version: String,
    val timestamp: String = "",
    val plugins: List<String> = emptyList(),
)

data class AgentInfo(
    val agent_id: String,
    val goal_id: String,
    val tag: String = "",
    val title: String = "",
    val state: String,
    val running_secs: Long = 0,
    val active: Boolean = true,
)

data class ProjectStatus(
    val project: String = "",
    val version: String = "",
    val daemon_version: String = "",
    val active_agents: List<AgentInfo> = emptyList(),
    val pending_drafts: Int = 0,
    val active_goals: Int = 0,
    val total_goals: Int = 0,
)

data class DraftSummary(
    val package_id: String,
    val title: String = "",
    val status: String,
    val created_at: String = "",
    val artifact_count: Int = 0,
    val goal_id: String? = null,
)

data class CmdResponse(
    val exit_code: Int,
    val stdout: String = "",
    val stderr: String = "",
)

data class ActionResponse(
    val package_id: String = "",
    val status: String = "",
    val message: String = "",
)

class TaDaemonClient(
    private val baseUrl: String,
    private val token: String = "",
) {
    private val gson = Gson()
    private val http = HttpClient.newBuilder()
        .connectTimeout(Duration.ofSeconds(10))
        .build()

    private fun requestBuilder(path: String): HttpRequest.Builder {
        val builder = HttpRequest.newBuilder()
            .uri(URI.create("$baseUrl$path"))
            .timeout(Duration.ofSeconds(15))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
        if (token.isNotBlank()) {
            builder.header("Authorization", "Bearer $token")
        }
        return builder
    }

    private inline fun <reified T> get(path: String): T {
        val type = object : TypeToken<T>() {}.type
        val req = requestBuilder(path).GET().build()
        val res = http.send(req, HttpResponse.BodyHandlers.ofString())
        if (res.statusCode() >= 400) {
            throw RuntimeException("HTTP ${res.statusCode()}: ${res.body().take(200)}")
        }
        return gson.fromJson(res.body(), type)
    }

    private inline fun <reified T> post(path: String, body: Any): T {
        val type = object : TypeToken<T>() {}.type
        val json = gson.toJson(body)
        val req = requestBuilder(path)
            .POST(HttpRequest.BodyPublishers.ofString(json))
            .build()
        val res = http.send(req, HttpResponse.BodyHandlers.ofString())
        if (res.statusCode() >= 400) {
            throw RuntimeException("HTTP ${res.statusCode()}: ${res.body().take(200)}")
        }
        return gson.fromJson(res.body(), type)
    }

    fun health(): HealthResponse = get("/health")

    fun getStatus(): ProjectStatus = get("/api/status")

    fun listDrafts(): List<DraftSummary> = get("/api/drafts")

    fun approveDraft(id: String): ActionResponse = post("/api/drafts/$id/approve", emptyMap<String, String>())

    fun denyDraft(id: String, reason: String): ActionResponse =
        post("/api/drafts/$id/deny", mapOf("reason" to reason))

    fun runCommand(command: String): CmdResponse = post("/api/cmd", mapOf("command" to command))

    /**
     * Opens an SSE stream and calls [onEvent] for each event.
     * Returns a [Closeable] — call close() to stop the stream.
     * The stream runs on a daemon background thread.
     */
    fun openEventStream(
        onEvent: (type: String, data: String) -> Unit,
        onError: (err: Exception) -> Unit,
        since: String? = null,
        types: String? = null,
    ): Closeable {
        val params = buildList {
            since?.let { add("since=$it") }
            types?.let { add("types=$it") }
        }.joinToString("&").let { if (it.isNotEmpty()) "?$it" else "" }

        val req = HttpRequest.newBuilder()
            .uri(URI.create("$baseUrl/api/events$params"))
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .apply { if (token.isNotBlank()) header("Authorization", "Bearer $token") }
            .GET()
            .build()

        @Volatile var stopped = false

        val thread = Thread {
            try {
                val res = http.send(req, HttpResponse.BodyHandlers.ofInputStream())
                res.body().bufferedReader().use { reader ->
                    var eventType = "message"
                    var data = ""
                    reader.forEachLine { line ->
                        if (stopped) return@forEachLine
                        when {
                            line.startsWith("event:") -> eventType = line.removePrefix("event:").trim()
                            line.startsWith("data:") -> data = line.removePrefix("data:").trim()
                            line.isEmpty() && data.isNotEmpty() -> {
                                onEvent(eventType, data)
                                eventType = "message"
                                data = ""
                            }
                        }
                    }
                }
            } catch (e: Exception) {
                if (!stopped) onError(e)
            }
        }.apply {
            isDaemon = true
            name = "ta-sse-listener"
            start()
        }

        return Closeable {
            stopped = true
            thread.interrupt()
        }
    }
}
