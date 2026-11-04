package com.lecer.lisapp

import android.app.Activity
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.CancellationSignal
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Log
import java.security.GeneralSecurityException
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.spec.ECGenParameterSpec

/**
 * The key that unlocks the PC. It's made in the phone's secure hardware and can't be read out. It
 * signs only right after a fingerprint or face check, and stops working if one is added, so taking
 * the app's files or adding a fingerprint to a stolen phone doesn't help.
 */
object Unlocker {
    private const val TAG = "LisUnlock"
    private const val KEY_STORE = "AndroidKeyStore"
    private const val ALIAS = "lis_pc_unlock"
    private const val ALGORITHM = "SHA256withECDSA"

    private fun keyStore(): KeyStore = KeyStore.getInstance(KEY_STORE).apply { load(null) }

    fun hasKey(): Boolean = runCatching { keyStore().containsAlias(ALIAS) }.getOrDefault(false)

    /** Why this phone can't do it, or null when it can. */
    fun problem(activity: Activity): String? = when (activity.getSystemService(BiometricManager::class.java)?.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG)) {
        BiometricManager.BIOMETRIC_SUCCESS -> null
        BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED -> "Add a fingerprint to this phone first"
        else -> "This phone has no secure fingerprint sensor"
    }

    /** The key's public half, made the first time. */
    fun publicKey(): ByteArray {
        keyStore().getCertificate(ALIAS)?.let { return it.publicKey.encoded }
        val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)
            .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
            .setInvalidatedByBiometricEnrollment(true)
            .build()
        val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, KEY_STORE)
        generator.initialize(spec)
        return generator.generateKeyPair().public.encoded
    }

    fun deleteKey() {
        runCatching { keyStore().deleteEntry(ALIAS) }.onFailure { Log.w(TAG, "can't delete the key", it) }
    }

    /**
     * Signs the message after asking for a fingerprint. Calls back on the main thread. `failed` says
     * whether it was the prompt that failed, rather than the key.
     */
    fun sign(
        activity: Activity,
        pcName: String,
        message: ByteArray,
        cancel: CancellationSignal,
        done: (ByteArray) -> Unit,
        failed: (String, Boolean) -> Unit,
        cancelled: () -> Unit,
    ) {
        val signature = try {
            val key = keyStore().getKey(ALIAS, null) as? PrivateKey ?: return failed("Turn on fingerprint unlock first", false)
            Signature.getInstance(ALGORITHM).apply { initSign(key) }
        } catch (_: KeyPermanentlyInvalidatedException) {
            deleteKey()
            Lis.update { copy(unlockKey = false) }
            return failed("This phone's fingerprints changed, so turn fingerprint unlock on again", false)
        } catch (error: GeneralSecurityException) {
            Log.w(TAG, "can't use the key", error)
            return failed("Can't use the key: ${error.message}", false)
        }

        val prompt = BiometricPrompt.Builder(activity)
            .setTitle("Unlock $pcName")
            .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
            .setNegativeButton("Cancel", activity.mainExecutor) { _, _ -> cancelled() }
            .build()
        val callback = object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                try {
                    val signer = result.cryptoObject?.signature ?: return failed("The phone didn't approve the key", false)
                    signer.update(message)
                    done(signer.sign())
                } catch (error: GeneralSecurityException) {
                    Log.w(TAG, "can't sign", error)
                    failed("Can't sign: ${error.message}", false)
                }
            }

            override fun onAuthenticationError(code: Int, text: CharSequence) {
                if (code == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED || code == BiometricPrompt.BIOMETRIC_ERROR_TIMEOUT) {
                    cancelled()
                } else {
                    failed(text.toString(), true)
                }
            }
        }
        prompt.authenticate(BiometricPrompt.CryptoObject(signature), cancel, activity.mainExecutor, callback)
    }
}
