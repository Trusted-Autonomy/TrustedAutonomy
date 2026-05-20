package com.trustedautonomy.ta

import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.content.ContentFactory

class TaToolWindowFactory : ToolWindowFactory, DumbAware {

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val contentFactory = ContentFactory.getInstance()

        val goalsPanel = GoalsPanel(project)
        val goalsContent = contentFactory.createContent(goalsPanel, "Goals", false)
        goalsContent.setDisposer(Disposer.newDisposable("ta-goals").also {
            Disposer.register(it) { goalsPanel.dispose() }
        })

        val draftsPanel = DraftsPanel(project)
        val draftsContent = contentFactory.createContent(draftsPanel, "Drafts", false)
        draftsContent.setDisposer(Disposer.newDisposable("ta-drafts").also {
            Disposer.register(it) { draftsPanel.dispose() }
        })

        toolWindow.contentManager.addContent(goalsContent)
        toolWindow.contentManager.addContent(draftsContent)
    }

    override fun shouldBeAvailable(project: Project): Boolean = true
}
