package com.crispstrobe.crispsorter

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract

/**
 * SAF (Storage Access Framework) bridge for CrispSorter.
 *
 * Called from Rust via JNI to perform file operations on content:// URIs
 * that the Android scoped storage model requires.  The Rust side obtains
 * a tree URI from the Tauri dialog plugin's folder picker, then calls
 * these methods for list / read / move / mkdir / delete within that tree.
 */
object SAFBridge {

    /**
     * List children of a tree URI.  Returns a JSON array of objects with
     * keys: uri, displayName, mimeType, size, isDirectory.
     */
    @JvmStatic
    fun listFolder(context: Context, treeUriString: String): String {
        val treeUri = Uri.parse(treeUriString)
        val docId = DocumentsContract.getTreeDocumentId(treeUri)
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, docId)

        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
        )

        val items = mutableListOf<String>()
        context.contentResolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
            val idCol = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_DOCUMENT_ID)
            val nameCol = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
            val mimeCol = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_MIME_TYPE)
            val sizeCol = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_SIZE)

            while (cursor.moveToNext()) {
                val childDocId = cursor.getString(idCol)
                val childUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, childDocId)
                val name = cursor.getString(nameCol) ?: ""
                val mime = cursor.getString(mimeCol) ?: "*/*"
                val size = if (sizeCol >= 0) cursor.getLong(sizeCol) else -1L
                val isDir = mime == DocumentsContract.Document.MIME_TYPE_DIR

                // Emit JSON manually to avoid pulling in a JSON library
                items.add("""{"uri":"${escJson(childUri.toString())}","displayName":"${escJson(name)}","mimeType":"${escJson(mime)}","size":$size,"isDirectory":$isDir}""")
            }
        }
        return "[${items.joinToString(",")}]"
    }

    /**
     * Read a document's bytes.  Returns the raw byte array.
     */
    @JvmStatic
    fun readFile(context: Context, uriString: String): ByteArray {
        val uri = Uri.parse(uriString)
        return context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw IllegalStateException("Cannot open input stream for $uriString")
    }

    /**
     * Move a document within a tree.  Returns the new URI string.
     * Requires API 24+ (DocumentsContract.moveDocument).
     */
    @JvmStatic
    fun moveDocument(
        context: Context,
        sourceUriString: String,
        sourceParentUriString: String,
        targetParentUriString: String,
    ): String {
        val sourceUri = Uri.parse(sourceUriString)
        val sourceParent = Uri.parse(sourceParentUriString)
        val targetParent = Uri.parse(targetParentUriString)
        val newUri = DocumentsContract.moveDocument(
            context.contentResolver, sourceUri, sourceParent, targetParent,
        ) ?: throw IllegalStateException("moveDocument returned null")
        return newUri.toString()
    }

    /**
     * Create a subfolder.  Returns the new folder's URI string.
     */
    @JvmStatic
    fun createDirectory(context: Context, parentUriString: String, name: String): String {
        val parentUri = Uri.parse(parentUriString)
        val newUri = DocumentsContract.createDocument(
            context.contentResolver,
            parentUri,
            DocumentsContract.Document.MIME_TYPE_DIR,
            name,
        ) ?: throw IllegalStateException("createDocument returned null for dir '$name'")
        return newUri.toString()
    }

    /**
     * Delete a document or folder.
     */
    @JvmStatic
    fun deleteDocument(context: Context, uriString: String): Boolean {
        val uri = Uri.parse(uriString)
        return DocumentsContract.deleteDocument(context.contentResolver, uri)
    }

    private fun escJson(s: String): String =
        s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n")
}
