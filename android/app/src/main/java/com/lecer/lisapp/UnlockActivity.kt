package com.lecer.lisapp

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.view.MotionEvent
import android.view.WindowManager
import android.widget.Toast
import uniffi.lis_ffi.ConnectionState

/**
 * The fingerprint prompt for unlocking the PC, over whatever is on the phone, the lock screen too.
 * A prompt needs an activity in front, so this is an invisible one that only lasts as long as it.
 */
class UnlockActivity : Activity() {
    private val main = Handler(Looper.getMainLooper())
    private val cancel = CancellationSignal()
    private var dismissingKeyguard = false
    private var triedKeyguard = false
    private val giveUp = Runnable { finish() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = this
        Notifications.cancelUnlockPrompt(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        // Only an opaque activity surely takes over from the lock screen, which would otherwise take
        // the fingerprint for itself
        if (getSystemService(KeyguardManager::class.java).isKeyguardLocked) {
            setTranslucent(false)
            window.setBackgroundDrawable(ColorDrawable(Color.BLACK))
        }

        if (Lis.state.value.connection != ConnectionState.CONNECTED) {
            over("Not connected to the PC")
            return
        }
        Lis.update { copy(unlockMessage = null) }
        Lis.onUnlockChallenge = { nonce -> runOnUiThread { ask(nonce) } }
        Lis.onUnlockOver = { message -> runOnUiThread { over(message) } }
        // This can't be seen, so it must never be left in the way
        main.postDelayed(giveUp, NO_ANSWER_MS)
        Lis.client.unlockRequest()
    }

    private fun ask(nonce: String) {
        main.removeCallbacks(giveUp)
        main.postDelayed(giveUp, PROMPT_LIMIT_MS)
        val message = Lis.client.unlockMessage(nonce) ?: return over("Not paired with a PC")
        Unlocker.sign(
            activity = this,
            pcName = Lis.state.value.paired?.name ?: "your PC",
            message = message,
            cancel = cancel,
            done = { signature ->
                main.removeCallbacks(giveUp)
                main.postDelayed(giveUp, NO_ANSWER_MS)
                Lis.client.unlockRespond(nonce, signature)
            },
            failed = { reason, promptFailed -> if (promptFailed) promptFailed(nonce, reason) else over(reason) },
            cancelled = ::finish,
        )
    }

    /** Some phones may not show a prompt over the lock screen, so the phone gets unlocked first. */
    private fun promptFailed(nonce: String, reason: String) {
        val keyguard = getSystemService(KeyguardManager::class.java)
        if (isFinishing || triedKeyguard || !keyguard.isKeyguardLocked) return over(reason)
        Log.i(TAG, "no prompt over the lock screen: $reason")
        triedKeyguard = true
        dismissingKeyguard = true
        keyguard.requestDismissKeyguard(this, object : KeyguardManager.KeyguardDismissCallback() {
            override fun onDismissSucceeded() {
                dismissingKeyguard = false
                ask(nonce)
            }

            override fun onDismissCancelled() = finish()

            override fun onDismissError() = finish()
        })
    }

    private fun over(message: String?) {
        if (isFinishing) return
        message?.let { Toast.makeText(applicationContext, it, Toast.LENGTH_LONG).show() }
        finish()
    }

    /** Nothing shows, so a touch that reaches this was meant for what's behind it. */
    override fun onTouchEvent(event: MotionEvent): Boolean {
        finish()
        return true
    }

    /** The screen went off, or the person moved on. */
    override fun onStop() {
        super.onStop()
        if (!dismissingKeyguard) finish()
    }

    override fun onDestroy() {
        main.removeCallbacksAndMessages(null)
        cancel.cancel()
        // A new one may already have taken over
        if (current === this) {
            current = null
            Lis.onUnlockChallenge = null
            Lis.onUnlockOver = null
        }
        super.onDestroy()
    }

    companion object {
        private const val TAG = "LisUnlock"
        private const val NO_ANSWER_MS = 5_000L
        /** Well inside how long the PC's challenge lasts, as a late answer counts against the phone. */
        private const val PROMPT_LIMIT_MS = 30_000L

        private var current: UnlockActivity? = null

        fun intent(context: Context): Intent =
            Intent(context, UnlockActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

        /**
         * When the PC wants unlocking. A full-screen notification opens this when the phone is locked
         * or asleep, and while it's in use it only shows as a banner, so with permission to draw over
         * other apps this also starts directly. HyperOS can block that without saying so, which leaves
         * the banner.
         */
        fun popUp(context: Context) {
            val state = Lis.state.value
            // Not over the camera's framing
            if (current != null || state.camera.running || !Unlocker.hasKey()) return
            Notifications.showUnlockPrompt(context, state.paired?.name ?: "your PC")
            if (Settings.canDrawOverlays(context)) {
                runCatching { context.startActivity(intent(context)) }.onFailure { Log.w(TAG, "can't start the prompt", it) }
            }
        }
    }
}
