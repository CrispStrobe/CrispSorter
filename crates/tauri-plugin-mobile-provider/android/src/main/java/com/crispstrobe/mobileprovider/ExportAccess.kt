package com.crispstrobe.mobileprovider

import android.content.Context
import android.net.Uri
import java.io.File
import java.util.UUID

/** Only prepared OCR exports may cross into a destination chosen by the user. */
internal class ExportAccess(private val context: Context) {
    companion object { const val MAX_BYTES = 256L * 1024 * 1024 }

    fun source(path: String, name: String): File {
        require(name.isNotEmpty() && name.length <= 240 && name != "." && name != ".." &&
            name.none { it == '/' || it == '\\' || it.isISOControl() }) { "Invalid export filename" }
        val cache = context.cacheDir.canonicalFile
        val expectedRoot = File(cache, "ocr-exports")
        val root = expectedRoot.canonicalFile
        require(root == expectedRoot) { "Export staging must remain in the application cache" }
        val file = File(path).canonicalFile
        val directory = file.parentFile ?: throw IllegalArgumentException("Invalid export path")
        require(directory.parentFile == root && UUID.fromString(directory.name).toString() == directory.name) {
            "Only prepared OCR export files may be saved"
        }
        require(file.name == name && file.isFile && file.length() <= MAX_BYTES) { "Export file is unavailable or exceeds 256 MiB" }
        return file
    }

    fun copy(source: File, destination: Uri): Long {
        require(destination.scheme == "content" && !destination.authority.isNullOrEmpty()) { "Expected a destination from the save picker" }
        return source.inputStream().use { input ->
            val stream = context.contentResolver.openOutputStream(destination, "wt")
                ?: throw IllegalStateException("The selected provider could not create the export")
            stream.use { output ->
                val buffer = ByteArray(64 * 1024)
                var total = 0L
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    total += count
                    check(total <= MAX_BYTES) { "Export exceeds 256 MiB" }
                    output.write(buffer, 0, count)
                }
                output.flush()
                total
            }
        }
    }
}
