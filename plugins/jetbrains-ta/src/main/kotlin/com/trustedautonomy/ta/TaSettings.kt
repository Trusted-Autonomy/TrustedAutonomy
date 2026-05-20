package com.trustedautonomy.ta

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage

@State(name = "TaSettings", storages = [Storage("ta-plugin.xml")])
@Service(Service.Level.APP)
class TaSettings : PersistentStateComponent<TaSettings.State> {

    data class State(
        var daemonUrl: String = "http://127.0.0.1:7700",
        var apiToken: String = "",
        var pollIntervalSeconds: Int = 15,
    )

    private var myState = State()

    override fun getState(): State = myState

    override fun loadState(state: State) {
        myState = state
    }

    fun newClient(): TaDaemonClient = TaDaemonClient(myState.daemonUrl, myState.apiToken)

    companion object {
        @JvmStatic
        fun getInstance(): TaSettings =
            ApplicationManager.getApplication().getService(TaSettings::class.java)
    }
}
