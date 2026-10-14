package com.matthewdovi.audionet

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import org.json.JSONArray
import org.json.JSONObject
import uniffi.audionet_ffi.Account
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Where this phone's AudioNet memberships are kept: for each account it is
 * signed in to, the server, device id, device token and names. Account
 * passwords are never stored; each device token is revocable (signing out,
 * or removing the device on the web page, ends it).
 *
 * The list is a file in the app's private storage (no other app can read
 * it), encrypted with AES-GCM under a key that lives in the Android
 * Keystore and never leaves it. It is not backed up (allowBackup is off):
 * a restored phone signs in again.
 */
class AccountStore(context: Context) {
    private val file = File(context.filesDir, "accounts.bin")

    /** The app's settings (sharing choices, Show Measurements). */
    val settings: SharedPreferences = context.getSharedPreferences("audionet", Context.MODE_PRIVATE)

    val hasStoredAccount: Boolean get() = file.exists()

    /** The stored accounts, and why they could not be read (in words). */
    fun loadAll(): Pair<List<Account>, String?> {
        if (!file.exists()) return emptyList<Account>() to null
        return try {
            val bytes = file.readBytes()
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0, 12)))
            val json = JSONArray(String(cipher.doFinal(bytes, 12, bytes.size - 12), Charsets.UTF_8))
            (0 until json.length()).map { i ->
                val o = json.getJSONObject(i)
                Account(o.getString("server"), o.getString("node_id"), o.getString("token"),
                    o.getString("device_name"), o.getString("username"))
            } to null
        } catch (e: Exception) {
            emptyList<Account>() to "AudioNet could not read this phone's sign-ins (${e.message ?: e.javaClass.simpleName}). Sign in again."
        }
    }

    /** Writes the accounts; returns why it failed, in words. */
    fun saveAll(accounts: List<Account>): String? = try {
        if (accounts.isEmpty()) {
            file.delete()
        } else {
            val json = JSONArray()
            for (a in accounts) {
                json.put(JSONObject()
                    .put("server", a.serverUrl).put("node_id", a.nodeId).put("token", a.token)
                    .put("device_name", a.deviceName).put("username", a.username))
            }
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, key())
            val sealed = cipher.iv + cipher.doFinal(json.toString().toByteArray(Charsets.UTF_8))
            val temp = File(file.parentFile, "accounts.tmp")
            temp.writeBytes(sealed)
            if (!temp.renameTo(file)) {
                file.delete()
                temp.renameTo(file)
            }
        }
        null
    } catch (e: Exception) {
        "AudioNet could not save this phone's sign-ins: ${e.message ?: e.javaClass.simpleName}"
    }

    /** The Keystore key the sign-ins are sealed with, made once. */
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build())
        return generator.generateKey()
    }

    private companion object {
        const val KEY_ALIAS = "audionet-accounts"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
    }
}

/** The account in words: "alice on audionet.example.com". */
val Account.accountName: String
    get() = "$username on ${android.net.Uri.parse(serverUrl).host ?: serverUrl}"

/** Whether `other` is the same account (same server and name). */
fun Account.sameAccount(other: Account): Boolean =
    username.equals(other.username, ignoreCase = true) &&
        serverUrl.trimEnd('/').equals(other.serverUrl.trimEnd('/'), ignoreCase = true)
