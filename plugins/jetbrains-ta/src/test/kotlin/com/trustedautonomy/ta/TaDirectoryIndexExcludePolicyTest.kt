package com.trustedautonomy.ta

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class TaDirectoryIndexExcludePolicyTest {

    @Test
    fun jetbrains_policy_falls_back_to_hardcoded_when_no_manifest(@TempDir taDir: Path) {
        val result = TaDirectoryIndexExcludePolicy.readManifestDirs(taDir.toFile())
        assertNull(result, "readManifestDirs should return null when ide-excludes.json is absent")
    }

    @Test
    fun jetbrains_policy_reads_manifest(@TempDir taDir: Path) {
        val manifest = """
            {
                "version": 1,
                "ta_dir": ".ta",
                "dirs": ["staging/", "goals/", "sessions/", "memory/"]
            }
        """.trimIndent()
        File(taDir.toFile(), "ide-excludes.json").writeText(manifest)

        val dirs = TaDirectoryIndexExcludePolicy.readManifestDirs(taDir.toFile())
        assertNotNull(dirs, "should read dirs from manifest")
        assertEquals(4, dirs.size)
        // Trailing slashes should be stripped for File path construction.
        assertTrue(dirs.contains("staging"), "should contain 'staging' (slash stripped)")
        assertTrue(dirs.contains("goals"), "should contain 'goals' (slash stripped)")
        assertTrue(dirs.contains("sessions"), "should contain 'sessions' (slash stripped)")
        assertTrue(dirs.contains("memory"), "should contain 'memory' (slash stripped)")
    }

    @Test
    fun readManifestDirs_returns_null_for_malformed_json(@TempDir taDir: Path) {
        File(taDir.toFile(), "ide-excludes.json").writeText("not valid json!!")
        val result = TaDirectoryIndexExcludePolicy.readManifestDirs(taDir.toFile())
        assertNull(result, "malformed JSON should return null so caller falls back to hardcoded list")
    }

    @Test
    fun readManifestDirs_returns_null_for_missing_dirs_key(@TempDir taDir: Path) {
        File(taDir.toFile(), "ide-excludes.json").writeText("""{"version": 1, "ta_dir": ".ta"}""")
        val result = TaDirectoryIndexExcludePolicy.readManifestDirs(taDir.toFile())
        // Manifest without "dirs" key should fall back gracefully.
        assertNull(result, "manifest without 'dirs' key should return null")
    }

    @Test
    fun fallback_list_contains_all_expected_dirs() {
        val required = listOf(
            "staging", "store", "goals", "events", "sessions",
            "interactions", "pr_packages", "review", "backups",
            "heartbeats", "workflow-runs", "advisor-notes", "draft-build-ctx",
            "memory", "link-cache"
        )
        for (dir in required) {
            assertTrue(
                TaDirectoryIndexExcludePolicy.TA_RUNTIME_DIRS.contains(dir),
                "TA_RUNTIME_DIRS should contain '$dir'"
            )
        }
    }
}
