package com.trustedautonomy.ta

import com.google.gson.JsonParseException
import com.google.gson.JsonParser
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.impl.DirectoryIndexExcludePolicy
import com.intellij.openapi.vfs.VfsUtil

/**
 * Excludes TA runtime directories from IntelliJ's file index and symbol search.
 *
 * `.ta/staging/` is a full copy of the workspace — without exclusion every symbol appears
 * twice, every file has a shadow copy, and search results are polluted with staging artifacts.
 * `.ta/goals/` and `.ta/sessions/` contain JSONL event logs that have no value in IDE search.
 *
 * Reads `.ta/ide-excludes.json` (written by `ta init` / `ta doctor --fix`) at call time and
 * uses its `dirs` array as the authoritative exclude list. Falls back to the hardcoded
 * [TA_RUNTIME_DIRS] list when the file is absent (projects not yet re-initialized) or
 * malformed.
 *
 * Registered under `com.intellij.directoryIndexExcludePolicy` in plugin.xml. The policy runs
 * at IDE startup and applies dynamically — no `.idea/` file changes are required.
 */
class TaDirectoryIndexExcludePolicy(private val project: Project) : DirectoryIndexExcludePolicy {

    companion object {
        /**
         * Fallback runtime directories inside `.ta/` used when `.ta/ide-excludes.json` is absent.
         * Kept in sync with LOCAL_TA_PATHS in crates/ta-workspace/src/partitioning.rs.
         */
        internal val TA_RUNTIME_DIRS = listOf(
            "staging",
            "store",
            "goals",
            "events",
            "sessions",
            "interactions",
            "pr_packages",
            "review",
            "backups",
            "heartbeats",
            "workflow-runs",
            "advisor-notes",
            "draft-build-ctx",
            "memory",
            "link-cache"
        )

        /**
         * Read directory names from `.ta/ide-excludes.json`.
         *
         * Returns a list of directory names (trailing slash stripped, ready for [java.io.File]
         * path construction) when the manifest is present and parseable.
         * Returns null on any error so the caller can fall back to [TA_RUNTIME_DIRS].
         */
        internal fun readManifestDirs(taDir: java.io.File): List<String>? {
            val manifestFile = java.io.File(taDir, "ide-excludes.json")
            if (!manifestFile.exists()) return null
            return try {
                val root = JsonParser.parseString(manifestFile.readText())
                if (!root.isJsonObject) return null
                val dirsArray = root.asJsonObject.getAsJsonArray("dirs") ?: return null
                dirsArray.mapNotNull { element ->
                    if (element.isJsonPrimitive) element.asString.trimEnd('/') else null
                }
            } catch (_: JsonParseException) {
                null
            } catch (_: Exception) {
                null
            }
        }
    }

    override fun getExcludeUrlsForProject(): Array<String> {
        val basePath = project.basePath ?: return emptyArray()
        val taDir = java.io.File(basePath, ".ta")
        if (!taDir.exists() || !taDir.isDirectory) return emptyArray()

        val dirs = readManifestDirs(taDir) ?: TA_RUNTIME_DIRS

        return dirs
            .map { java.io.File(taDir, it) }
            .filter { it.exists() && it.isDirectory }
            .mapNotNull { VfsUtil.pathToUrl(it.absolutePath) }
            .toTypedArray()
    }
}
