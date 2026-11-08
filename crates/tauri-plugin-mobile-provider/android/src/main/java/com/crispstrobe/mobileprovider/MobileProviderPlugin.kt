package com.crispstrobe.mobileprovider

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.util.Base64
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import kotlin.concurrent.thread

@InvokeArg
class ReadArgs {
    lateinit var uri: String
}

/** A deliberately read-only SAF bridge. No content URI is converted to a path. */
@TauriPlugin
class MobileProviderPlugin(private val activity: Activity) : Plugin(activity) {
    companion object {
        private const val MAX_ENTRIES = 5000
    }

    private val reader = SafReader(activity)

    @Command
    fun pickFiles(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            }
            startActivityForResult(invoke, intent, "filesPicked")
        } catch (e: Exception) {
            invoke.reject("Could not open the document picker: ${e.message}")
        }
    }

    @Command
    fun pickFolder(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            }
            startActivityForResult(invoke, intent, "folderPicked")
        } catch (e: Exception) {
            invoke.reject("Could not open the folder picker: ${e.message}")
        }
    }

    @ActivityCallback
    fun filesPicked(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) {
            invoke.resolve(JSObject().put("cancelled", true).put("files", JSArray()))
            return
        }
        if (result.resultCode != Activity.RESULT_OK || result.data == null) {
            invoke.reject("The document picker did not return a selection")
            return
        }
        val data = result.data!!
        val uris = mutableListOf<Uri>()
        val clip = data.clipData
        if (clip != null) {
            for (index in 0 until clip.itemCount) uris.add(clip.getItemAt(index).uri)
        } else {
            data.data?.let { uris.add(it) }
        }
        runInBackground(invoke) {
            if (uris.isEmpty()) throw IllegalStateException("The picker returned no documents")
            if (uris.size > MAX_ENTRIES) throw IllegalStateException("Select at most $MAX_ENTRIES documents at once")
            val files = JSArray()
            for (uri in uris) {
                persistReadGrant(uri, data.flags)
                files.put(reader.queryFile(uri))
            }
            JSObject().put("cancelled", false).put("files", files)
        }
    }

    @ActivityCallback
    fun folderPicked(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) {
            invoke.resolve(JSObject().put("cancelled", true).put("files", JSArray()))
            return
        }
        if (result.resultCode != Activity.RESULT_OK || result.data?.data == null) {
            invoke.reject("The folder picker did not return a selection")
            return
        }
        val data = result.data!!
        val tree = data.data!!
        runInBackground(invoke) {
            persistReadGrant(tree, data.flags)
            val files = reader.listTree(tree)
            JSObject().put("cancelled", false).put("files", files)
        }
    }

    @Command
    fun readFile(invoke: Invoke) {
        val args = try { invoke.parseArgs(ReadArgs::class.java) } catch (e: Exception) {
            invoke.reject("A document URI is required")
            return
        }
        runInBackground(invoke) {
            val bytes = reader.readFile(args.uri)
            JSObject().put("data", Base64.encodeToString(bytes, Base64.NO_WRAP))
        }
    }

    private fun persistReadGrant(uri: Uri, flags: Int) {
        reader.contentUri(uri.toString())
        if (flags and Intent.FLAG_GRANT_READ_URI_PERMISSION == 0) {
            throw SecurityException("The provider did not grant read access")
        }
        // A batch is saved across launches. Refuse transient grants instead of
        // saving entries which will silently stop working after an app restart.
        activity.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }

    private fun runInBackground(invoke: Invoke, work: () -> JSObject) {
        thread(name = "mobile-provider") {
            try {
                invoke.resolve(work())
            } catch (e: Exception) {
                invoke.reject("Document provider access failed: ${e.message ?: e.javaClass.simpleName}")
            }
        }
    }
}
