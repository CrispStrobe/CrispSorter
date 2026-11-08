package com.crispstrobe.mobileprovider

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import java.io.ByteArrayOutputStream
import java.util.ArrayDeque

/** Provider document IDs and URIs are opaque: every operation stays with ContentResolver. */
internal class SafReader(private val context: Context) {
    companion object {
        const val MAX_BYTES = 16 * 1024 * 1024
        const val MAX_ENTRIES = 5000
        const val MAX_DEPTH = 32
    }

    fun readFile(value: String): ByteArray {
        val uri = contentUri(value)
        return context.contentResolver.openInputStream(uri)?.use { input ->
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(32 * 1024)
            var total = 0
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                total += count
                if (total > MAX_BYTES) throw IllegalStateException("Document exceeds the 16 MiB mobile import limit")
                output.write(buffer, 0, count)
            }
            output.toByteArray()
        } ?: throw IllegalStateException("The provider could not open this document")
    }

    fun contentUri(value: String): Uri {
        val uri = Uri.parse(value)
        if (uri.scheme != "content" || uri.authority.isNullOrEmpty()) {
            throw IllegalArgumentException("Expected a content URI from a document provider")
        }
        return uri
    }

    fun queryFile(uri: Uri): JSObject {
        val projection = arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
        val cursor = context.contentResolver.query(uri, projection, null, null, null)
            ?: throw IllegalStateException("The provider could not describe a selected document")
        return cursor.use {
            if (!it.moveToFirst()) throw IllegalStateException("The provider returned no document details")
            val nameCol = it.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            val sizeCol = it.getColumnIndex(OpenableColumns.SIZE)
            val name = if (nameCol >= 0) it.getString(nameCol) else null
            val size = if (sizeCol >= 0 && !it.isNull(sizeCol)) it.getLong(sizeCol) else -1L
            fileEntry(uri, name ?: "", size)
        }
    }

    fun listTree(tree: Uri): JSArray {
        val rootId = DocumentsContract.getTreeDocumentId(tree)
        val pending = ArrayDeque<Pair<String, Int>>()
        val seen = mutableSetOf<String>()
        val files = JSArray()
        var entries = 0
        pending.add(rootId to 0)
        while (pending.isNotEmpty()) {
            val (directoryId, depth) = pending.removeFirst()
            if (!seen.add(directoryId)) throw IllegalStateException("The provider returned a repeated folder")
            val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, directoryId)
            val projection = arrayOf(
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                DocumentsContract.Document.COLUMN_MIME_TYPE,
                DocumentsContract.Document.COLUMN_SIZE,
            )
            val cursor = context.contentResolver.query(children, projection, null, null, null)
                ?: throw IllegalStateException("The provider could not list a folder")
            cursor.use {
                val idCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID)
                val nameCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
                val mimeCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE)
                val sizeCol = it.getColumnIndex(DocumentsContract.Document.COLUMN_SIZE)
                while (it.moveToNext()) {
                    if (++entries > MAX_ENTRIES) throw IllegalStateException("Folder exceeds the $MAX_ENTRIES entry mobile import limit")
                    val id = it.getString(idCol) ?: throw IllegalStateException("The provider returned a document without an ID")
                    val name = it.getString(nameCol) ?: ""
                    val mime = it.getString(mimeCol)
                    if (mime == DocumentsContract.Document.MIME_TYPE_DIR) {
                        if (depth >= MAX_DEPTH) throw IllegalStateException("Folder exceeds the $MAX_DEPTH level mobile import limit")
                        pending.add(id to depth + 1)
                    } else {
                        val size = if (sizeCol >= 0 && !it.isNull(sizeCol)) it.getLong(sizeCol) else -1L
                        files.put(fileEntry(DocumentsContract.buildDocumentUriUsingTree(tree, id), name, size))
                    }
                }
            }
        }
        return files
    }

    private fun fileEntry(uri: Uri, name: String, size: Long): JSObject =
        JSObject().put("uri", uri.toString()).put("name", name).put("size", size)

}
