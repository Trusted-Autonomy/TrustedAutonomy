package com.trustedautonomy.ta

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
 * Registered under `com.intellij.directoryIndexExcludePolicy` in plugin.xml. The policy runs
 * at IDE startup and applies dynamically — no `.idea/` file changes are required.
 */
class TaDirectoryIndexExcludePolicy(private val project: Project) : DirectoryIndexExcludePolicy {

    companion object {
        /**
         * Runtime directories inside `.ta/` that should be excluded from IDE indexing.
         * Kept in sync with LOCAL_TA_PATHS in crates/ta-workspace/src/partitioning.rs.
         */
        private val TA_RUNTIME_DIRS = listOf(
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
    }

    override fun getExcludeUrlsForProject(): Array<String> {
        val basePath = project.basePath ?: return emptyArray()
        val taDir = java.io.File(basePath, ".ta")
        if (!taDir.exists() || !taDir.isDirectory) return emptyArray()

        return TA_RUNTIME_DIRS
            .map { java.io.File(taDir, it) }
            .filter { it.exists() && it.isDirectory }
            .mapNotNull { VfsUtil.pathToUrl(it.absolutePath) }
            .toTypedArray()
    }
}
