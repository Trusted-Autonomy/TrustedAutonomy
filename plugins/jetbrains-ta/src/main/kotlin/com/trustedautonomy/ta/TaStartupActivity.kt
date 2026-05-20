package com.trustedautonomy.ta

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.StartupActivity
import com.intellij.openapi.wm.WindowManager

class TaStartupActivity : StartupActivity.DumbAware {
    override fun runActivity(project: Project) {
        // Start the SSE notification listener for this project
        TaNotificationService.getInstance(project).start()

        // Kick off an initial status bar refresh
        ApplicationManager.getApplication().executeOnPooledThread {
            val widget = WindowManager.getInstance()
                .getStatusBar(project)
                ?.getWidget(TaStatusBarWidget.ID) as? TaStatusBarWidget
            widget?.update()
        }
    }
}
