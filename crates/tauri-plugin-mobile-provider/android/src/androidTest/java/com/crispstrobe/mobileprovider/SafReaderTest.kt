package com.crispstrobe.mobileprovider

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import kotlin.concurrent.thread

private const val AUTHORITY = "com.crispstrobe.mobileprovider.testdocs"

/** An actual ContentResolver endpoint with opaque, case-sensitive document IDs. */
class FakeDocumentsProvider : ContentProvider() {
    override fun onCreate() = true
    override fun getType(uri: Uri): String = "text/plain"
    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()

    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor {
        if (uri.pathSegments.lastOrNull() != "children") {
            return MatrixCursor(arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)).apply {
                addRow(arrayOf("Résumé.txt", null))
            }
        }
        val cursor = MatrixCursor(arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
        ))
        when (DocumentsContract.getDocumentId(uri)) {
            "root" -> {
                cursor.addRow(arrayOf("root/Case", "Résumé.txt", "text/plain", 4))
                cursor.addRow(arrayOf("root/case", "notes.txt", "text/plain", null))
                cursor.addRow(arrayOf("root/sub", "Sub", DocumentsContract.Document.MIME_TYPE_DIR, null))
            }
            "root/sub" -> cursor.addRow(arrayOf("root/sub/nested", "nested.txt", "text/plain", 4))
            "loop" -> cursor.addRow(arrayOf("loop", "Loop", DocumentsContract.Document.MIME_TYPE_DIR, null))
        }
        return cursor
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        val id = DocumentsContract.getDocumentId(uri)
        if (id == "denied") throw SecurityException("revoked grant")
        val (read, write) = ParcelFileDescriptor.createPipe()
        thread {
            try {
                ParcelFileDescriptor.AutoCloseOutputStream(write).use { out ->
                    if (id == "large") {
                        val block = ByteArray(32 * 1024)
                        repeat(SafReader.MAX_BYTES / block.size) { out.write(block) }
                        out.write(1)
                    } else out.write(byteArrayOf(97, 0, 98, 99))
                }
            } catch (_: Exception) { /* Reader intentionally closes an oversized pipe. */ }
        }
        return read
    }
}

@RunWith(AndroidJUnit4::class)
class SafReaderTest {
    private val reader = SafReader(ApplicationProvider.getApplicationContext())

    @Test fun folderTraversalPreservesTreeUrisAndCase() {
        val tree = DocumentsContract.buildTreeDocumentUri(AUTHORITY, "root")
        val files = reader.listTree(tree)
        assertEquals(3, files.length())
        val entries = (0 until files.length()).map { files.getJSONObject(it) }
        assertEquals(setOf("Résumé.txt", "notes.txt", "nested.txt"), entries.map { it.getString("name") }.toSet())
        val uris = entries.map { Uri.parse(it.getString("uri")) }
        assertEquals(setOf("root/Case", "root/case", "root/sub/nested"), uris.map { DocumentsContract.getDocumentId(it) }.toSet())
        assertTrue(uris.all { DocumentsContract.getTreeDocumentId(it) == "root" })
        assertEquals(-1L, entries.first { it.getString("name") == "notes.txt" }.getLong("size"))
    }

    @Test fun fileReadsUseProviderUriAndSupportUnknownSize() {
        val uri = DocumentsContract.buildDocumentUriUsingTree(DocumentsContract.buildTreeDocumentUri(AUTHORITY, "root"), "root/Case")
        val file = reader.queryFile(uri)
        assertEquals(uri.toString(), file.getString("uri"))
        assertEquals("Résumé.txt", file.getString("name"))
        assertEquals(-1L, file.getLong("size"))
        assertArrayEquals(byteArrayOf(97, 0, 98, 99), reader.readFile(uri.toString()))
    }

    @Test fun readLimitAndRevokedAccessProduceErrors() {
        fun uri(id: String) = DocumentsContract.buildDocumentUriUsingTree(DocumentsContract.buildTreeDocumentUri(AUTHORITY, "root"), id).toString()
        try {
            reader.readFile(uri("large"))
            fail("oversized document must be rejected")
        } catch (e: IllegalStateException) { assertTrue(e.message!!.contains("16 MiB")) }
        try {
            reader.readFile(uri("denied"))
            fail("revoked access must be reported")
        } catch (e: SecurityException) { assertTrue(e.message!!.contains("revoked")) }
    }

    @Test fun cyclicFolderIsAnErrorRatherThanAPartialImport() {
        val tree = DocumentsContract.buildTreeDocumentUri(AUTHORITY, "loop")
        try {
            reader.listTree(tree)
            fail("repeated folder must be rejected")
        } catch (e: IllegalStateException) { assertTrue(e.message!!.contains("repeated folder")) }
    }
}
