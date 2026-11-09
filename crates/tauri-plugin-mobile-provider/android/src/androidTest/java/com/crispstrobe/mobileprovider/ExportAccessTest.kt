package com.crispstrobe.mobileprovider

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.content.ContextWrapper
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.system.Os
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.util.UUID

private const val EXPORT_AUTHORITY = "com.crispstrobe.mobileprovider.testexports"

/** Real resolver writes, including revoked access and a broken output stream. */
class FakeExportProvider : ContentProvider() {
    override fun onCreate() = true
    override fun getType(uri: Uri): String = "application/octet-stream"
    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = if (file(uri).delete()) 1 else 0
    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor =
        MatrixCursor(arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)).apply {
            val file = file(uri)
            addRow(arrayOf(file.name, file.length()))
        }

    private fun file(uri: Uri): File {
        val id = DocumentsContract.getDocumentId(uri)
        require(id.matches(Regex("[a-z0-9-]+")))
        val root = File(context!!.cacheDir, "export-provider-tests").apply { mkdirs() }
        return File(root, id)
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        when (DocumentsContract.getDocumentId(uri)) {
            "denied" -> throw SecurityException("destination access revoked")
            "broken" -> {
                val pipe = ParcelFileDescriptor.createPipe()
                pipe[0].close()
                return pipe[1]
            }
        }
        if (mode == "r") return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_READ_ONLY)
        require(mode == "wt") { "Exports must request truncate semantics" }
        return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_CREATE or ParcelFileDescriptor.MODE_TRUNCATE or ParcelFileDescriptor.MODE_WRITE_ONLY)
    }
}

@RunWith(AndroidJUnit4::class)
class ExportAccessTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val original = byteArrayOf(97, 0, 98, -1, 10, 42)

    /** Each case owns a separate cache root, so symlink checks cannot affect other tests. */
    private fun isolated(test: (ExportAccess, File) -> Unit) {
        val cache = File(context.cacheDir, "export-check-${UUID.randomUUID()}").apply { mkdirs() }
        val wrapped = object : ContextWrapper(context) { override fun getCacheDir(): File = cache }
        try { test(ExportAccess(wrapped), cache) }
        finally { cache.deleteRecursively() }
    }

    private fun staged(cache: File, name: String = "Reviewed.pdf"): File {
        val directory = File(File(cache, "ocr-exports"), UUID.randomUUID().toString()).apply { mkdirs() }
        return File(directory, name).apply { writeBytes(original) }
    }

    private fun rejects(action: () -> Unit) {
        try { action(); fail("Export access should reject this source or destination") }
        catch (_: IllegalArgumentException) { }
    }

    @Test fun validatesOnlyPreparedNamedRegularFilesWithoutChangingSource() = isolated { access, cache ->
        val source = staged(cache)
        assertEquals(source.canonicalFile, access.source(source.path, source.name))
        assertArrayEquals(original, source.readBytes())
        for (name in listOf("", ".", "..", "../Reviewed.pdf", "bad\\name.pdf", "bad\nname.pdf", "x".repeat(241), "Different.pdf")) {
            rejects { access.source(source.path, name) }
        }
        val ordinary = File(cache, "ordinary.pdf").apply { writeBytes(original) }
        rejects { access.source(ordinary.path, ordinary.name) }
        val nonUuid = File(cache, "ocr-exports/not-a-uuid/Reviewed.pdf").apply { parentFile!!.mkdirs(); writeBytes(original) }
        rejects { access.source(nonUuid.path, nonUuid.name) }
        val missing = File(source.parentFile, "missing.pdf")
        rejects { access.source(missing.path, missing.name) }
        val directory = File(source.parentFile, "directory.pdf").apply { mkdirs() }
        rejects { access.source(directory.path, directory.name) }
    }

    @Test fun rejectsOversizedStagingWithoutReadingItsWholeContents() = isolated { access, cache ->
        val source = staged(cache)
        RandomAccessFile(source, "rw").use { it.setLength(ExportAccess.MAX_BYTES + 1) }
        rejects { access.source(source.path, source.name) }
        assertEquals(ExportAccess.MAX_BYTES + 1, source.length())
    }

    @Test fun rejectsFileAndDirectorySymlinksThatEscapeStaging() = isolated { access, cache ->
        val source = staged(cache)
        val outside = File(cache, "outside/Reviewed.pdf").apply { parentFile!!.mkdirs(); writeBytes(original) }
        source.delete()
        Os.symlink(outside.path, source.path)
        rejects { access.source(source.path, source.name) }
        source.delete()
        val directoryLink = File(cache, "ocr-exports/${UUID.randomUUID()}")
        Os.symlink(outside.parentFile!!.path, directoryLink.path)
        try { rejects { access.source(File(directoryLink, outside.name).path, outside.name) } }
        finally { directoryLink.delete() }
        assertArrayEquals(original, outside.readBytes())
    }

    @Test fun rejectsAnExportRootSymlinkOutsideTheCache() = isolated { access, cache ->
        val external = File(context.cacheDir, "outside-export-${UUID.randomUUID()}").apply { mkdirs() }
        val directory = File(external, UUID.randomUUID().toString()).apply { mkdirs() }
        val source = File(directory, "Reviewed.pdf").apply { writeBytes(original) }
        val link = File(cache, "ocr-exports")
        Os.symlink(external.path, link.path)
        try {
            rejects { access.source(File(File(link, directory.name), source.name).path, source.name) }
            assertArrayEquals(original, source.readBytes())
        } finally { link.delete(); external.deleteRecursively() }
    }

    @Test fun resolverCopyCompletesWithExactBytesAndPreservesSource() = isolated { access, cache ->
        val source = staged(cache)
        val destination = DocumentsContract.buildDocumentUri(EXPORT_AUTHORITY, "copy-${UUID.randomUUID()}")
        try {
            val bytes = access.copy(access.source(source.path, source.name), destination)
            assertEquals(original.size.toLong(), bytes)
            val written = context.contentResolver.openInputStream(destination)!!.use { it.readBytes() }
            assertArrayEquals(original, written)
            assertArrayEquals(original, source.readBytes())
        } finally { context.contentResolver.delete(destination, null, null) }
    }

    @Test fun providerOpenAndCopyErrorsNeverReportSuccessOrConsumeSource() = isolated { access, cache ->
        val source = staged(cache)
        rejects { access.copy(source, Uri.fromFile(source)) }
        rejects { access.copy(source, Uri.parse("https://example.com/output.pdf")) }
        try {
            access.copy(source, DocumentsContract.buildDocumentUri(EXPORT_AUTHORITY, "denied"))
            fail("Revoked destination access must be reported")
        } catch (error: SecurityException) { assertTrue(error.message!!.contains("revoked")) }
        try {
            access.copy(source, DocumentsContract.buildDocumentUri(EXPORT_AUTHORITY, "broken"))
            fail("A broken destination stream must be reported")
        } catch (_: IOException) { }
        assertArrayEquals(original, source.readBytes())
    }
}
