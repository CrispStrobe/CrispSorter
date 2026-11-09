package com.crispstrobe.mobileprovider

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ColorSpace
import android.graphics.ImageDecoder
import android.graphics.pdf.PdfRenderer
import android.os.Build
import android.os.ParcelFileDescriptor
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.OutputStream
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sqrt

/** Converts only archived, private OCR inputs. No provider URI or webview path is opened here. */
internal class DocumentRasterizer(private val context: Context, private val onProgress: ((Int, Int) -> Unit)? = null) {
    companion object {
        const val MAX_SOURCE_BYTES = 32L * 1024 * 1024
        const val MAX_PAGE_PIXELS = 16_000_000L
        const val MAX_SOURCE_PIXELS = 100_000_000L
        const val MAX_OUTPUT_BYTES = 512L * 1024 * 1024
        const val MAX_PAGES = 10_000
        const val PDF_DPI = 200.0

        fun capabilities(): JSObject {
            val formats = JSArray()
            if (Build.VERSION.SDK_INT >= 28) {
                if (ImageDecoder.isMimeTypeSupported("image/heif")) {
                    formats.put("heic"); formats.put("heif")
                }
                if (ImageDecoder.isMimeTypeSupported("image/avif")) formats.put("avif")
            }
            return JSObject().put("pdf", true).put("image_formats", formats)
                .put("max_source_bytes", MAX_SOURCE_BYTES).put("max_page_pixels", MAX_PAGE_PIXELS)
                .put("max_pages", MAX_PAGES)
        }

        fun boundedSize(width: Double, height: Double, preferredScale: Double): Pair<Int, Int> {
            require(width.isFinite() && height.isFinite() && width > 0 && height > 0) { "Invalid page dimensions" }
            val scale = min(preferredScale, min(16384.0 / max(width, height), sqrt(MAX_PAGE_PIXELS / width / height)))
            val w = max(1, floor(width * scale).toInt())
            val h = max(1, floor(height * scale).toInt())
            require(w.toLong() * h <= MAX_PAGE_PIXELS) { "Page exceeds the raster pixel limit" }
            return Pair(w, h)
        }
    }

    private data class Paths(val source: File, val output: File)

    private fun paths(sourcePath: String, outputDir: String): Paths {
        // Tauri's Android app_data_dir is Activity.dataDir (not filesDir).
        val home = context.dataDir.canonicalFile
        val workspace = File(home, "ocr-workspace")
        require(workspace.canonicalFile == workspace) { "The OCR workspace must not be a symbolic link" }
        val source = File(sourcePath)
        val output = File(outputDir)
        require(source.isAbsolute && source.canonicalFile == source && output.isAbsolute && output.canonicalFile == output) {
            "OCR import paths must be canonical private paths"
        }
        val sources = File(workspace, "sources")
        require(sources.canonicalFile == sources && source.parentFile == sources && source.isFile &&
            source.name.matches(Regex("[0-9a-f]{64}\\.(pdf|heic|heif|avif|png|jpe?g)"))) { "Invalid archived OCR source" }
        require(source.length() in 1..MAX_SOURCE_BYTES) { "This source exceeds the 32 MiB mobile import limit" }
        val staging = File(workspace, "import-staging")
        require(staging.canonicalFile == staging && output.parentFile == staging && output.isDirectory &&
            output.name.matches(Regex("import-[A-Za-z0-9_-]{6,100}"))) { "Invalid OCR import staging directory" }
        require(output.listFiles()?.all { it.name == "cancel" && it.isFile && it.canonicalFile == it } == true) {
            "The import staging directory is already in use"
        }
        return Paths(source, output)
    }

    private fun checkCancelled(output: File) {
        if (File(output, "cancel").exists()) throw IOException("OCR_IMPORT_CANCELLED")
    }

    private fun progress(output: File, completed: Int, total: Int) {
        checkCancelled(output)
        val pending = File(output, "progress.tmp")
        pending.writeText(JSObject().put("phase", "rendering").put("completed", completed).put("total", total).toString())
        check(pending.renameTo(File(output, "progress.json"))) { "Could not update import progress" }
        onProgress?.invoke(completed, total)
    }

    private class LimitedOutput(private val target: OutputStream, private val remaining: Long, private val output: File) : OutputStream() {
        var count = 0L
            private set
        override fun write(value: Int) { reserve(1); target.write(value); count++ }
        override fun write(bytes: ByteArray, offset: Int, length: Int) {
            reserve(length); target.write(bytes, offset, length); count += length
        }
        private fun reserve(length: Int) {
            if (File(output, "cancel").exists()) throw IOException("OCR_IMPORT_CANCELLED")
            if (length < 0 || count > remaining - length) throw IOException("Rendered document exceeds the 512 MiB import limit")
        }
        override fun flush() = target.flush()
    }

    private fun writePage(bitmap: Bitmap, output: File, index: Int, bytesWritten: Long): Pair<File, Long> {
        checkCancelled(output)
        val target = File(output, "page-%06d.png".format(java.util.Locale.ROOT, index + 1))
        check(target.createNewFile()) { "The page output already exists" }
        var size = 0L
        FileOutputStream(target).use { stream ->
            val limited = LimitedOutput(stream, MAX_OUTPUT_BYTES - bytesWritten, output)
            check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, limited)) { "Could not encode the rendered page" }
            limited.flush(); stream.fd.sync(); size = limited.count
        }
        checkCancelled(output)
        return Pair(target, size)
    }

    private fun page(path: File, bitmap: Bitmap, pointsWidth: Double, pointsHeight: Double, warnings: List<String>): JSObject {
        val notices = JSArray(); warnings.forEach { notices.put(it) }
        return JSObject().put("path", path.absolutePath).put("width", bitmap.width).put("height", bitmap.height)
            .put("width_points", pointsWidth).put("height_points", pointsHeight).put("warnings", notices)
    }

    fun rasterize(sourcePath: String, outputDir: String, kind: String): JSObject {
        val paths = paths(sourcePath, outputDir)
        checkCancelled(paths.output)
        return try {
            when (kind) {
                "pdf" -> pdf(paths)
                "image" -> image(paths)
                else -> throw IllegalArgumentException("Unsupported native document type")
            }
        } catch (_: OutOfMemoryError) {
            throw IllegalStateException("This document needs more memory than is available. Split the document or reduce the photo resolution and retry.")
        }
    }

    private fun pdf(paths: Paths): JSObject {
        val descriptor = ParcelFileDescriptor.open(paths.source, ParcelFileDescriptor.MODE_READ_ONLY)
        val renderer = try { PdfRenderer(descriptor) } catch (error: Exception) {
            descriptor.close()
            if (error is SecurityException) throw IOException("This PDF is password protected. Import an unlocked copy.", error)
            throw IOException("This PDF could not be opened; it may be damaged or password protected. Import an unlocked, valid PDF.", error)
        }
        renderer.use {
            require(renderer.pageCount in 1..MAX_PAGES) { "A PDF must contain 1 to $MAX_PAGES pages" }
            val pages = JSArray()
            var bytes = 0L
            progress(paths.output, 0, renderer.pageCount)
            for (index in 0 until renderer.pageCount) {
                checkCancelled(paths.output)
                renderer.openPage(index).use { sourcePage ->
                    require(sourcePage.width in 1..14400 && sourcePage.height in 1..14400) { "Page ${index + 1} has invalid or oversized physical dimensions" }
                    val (width, height) = boundedSize(sourcePage.width.toDouble(), sourcePage.height.toDouble(), PDF_DPI / 72)
                    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                    try {
                        bitmap.eraseColor(Color.WHITE)
                        sourcePage.render(bitmap, null, null, PdfRenderer.Page.RENDER_MODE_FOR_PRINT)
                        val (path, count) = writePage(bitmap, paths.output, index, bytes)
                        bytes += count
                        val warnings = if (width < floor(sourcePage.width * PDF_DPI / 72).toInt()) listOf("This page was reduced to the mobile raster limit; check small text before recognition.") else emptyList()
                        pages.put(page(path, bitmap, sourcePage.width.toDouble(), sourcePage.height.toDouble(), warnings))
                    } finally { bitmap.recycle() }
                }
                progress(paths.output, index + 1, renderer.pageCount)
            }
            return JSObject().put("pages", pages).put("bytes", bytes).put("decoder", "android-pdf-renderer")
        }
    }

    private fun image(paths: Paths): JSObject {
        require(Build.VERSION.SDK_INT >= 28) { "This Android version cannot decode this phone image format. Export a JPEG or PNG copy." }
        progress(paths.output, 0, 1)
        var originalWidth = 0
        var originalHeight = 0
        val decoded = ImageDecoder.decodeBitmap(ImageDecoder.createSource(paths.source)) { decoder, info, _ ->
            originalWidth = info.size.width; originalHeight = info.size.height
            require(originalWidth > 0 && originalHeight > 0 && originalWidth.toLong() * originalHeight <= MAX_SOURCE_PIXELS) { "The source image exceeds the 100 megapixel decode limit" }
            val (width, height) = boundedSize(originalWidth.toDouble(), originalHeight.toDouble(), 1.0)
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            decoder.setTargetSize(width, height)
            decoder.setTargetColorSpace(ColorSpace.get(ColorSpace.Named.SRGB))
            decoder.setOnPartialImageListener { false }
        }
        try {
            checkCancelled(paths.output)
            val opaque = Bitmap.createBitmap(decoded.width, decoded.height, Bitmap.Config.ARGB_8888)
            try {
                opaque.eraseColor(Color.WHITE)
                Canvas(opaque).drawBitmap(decoded, 0f, 0f, null)
                val (path, bytes) = writePage(opaque, paths.output, 0, 0)
                val warnings = mutableListOf("Imported the primary still image. Auxiliary images and animation frames are not included.")
                if (decoded.width < originalWidth || decoded.height < originalHeight) warnings.add("This photo was reduced to the mobile raster limit; the original file is retained.")
                val pages = JSArray(); pages.put(page(path, opaque, originalWidth * 72.0 / 300, originalHeight * 72.0 / 300, warnings))
                progress(paths.output, 1, 1)
                return JSObject().put("pages", pages).put("bytes", bytes).put("decoder", "android-image-decoder")
            } finally { opaque.recycle() }
        } finally { decoded.recycle() }
    }
}
