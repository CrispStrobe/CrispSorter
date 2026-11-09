package com.crispstrobe.mobileprovider

import android.content.Context
import android.content.ContextWrapper
import android.graphics.BitmapFactory
import android.graphics.Color
import android.os.Build
import android.system.Os
import android.util.Base64
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.RandomAccessFile
import java.security.MessageDigest
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class DocumentRasterizerTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val fixtures get() = InstrumentationRegistry.getInstrumentation().context.assets
    private fun digest(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    private fun fixture(name: String): ByteArray = fixtures.open(name).use { stream ->
        if (name.endsWith(".b64")) Base64.decode(stream.readBytes(), Base64.DEFAULT) else stream.readBytes()
    }
    private fun manifest() = JSONObject(fixtures.open("manifest.json").bufferedReader().use { it.readText() })

    private fun isolated(test: (Context, File) -> Unit) {
        val home = File(context.cacheDir, "document-check-${UUID.randomUUID()}").apply { mkdirs() }.canonicalFile
        val wrapped = object : ContextWrapper(context) { override fun getDataDir() = home }
        try { test(wrapped, home) } finally { home.deleteRecursively() }
    }
    private fun archive(home: File, bytes: ByteArray, extension: String): File =
        File(home, "ocr-workspace/sources/${digest(bytes)}.$extension").apply { parentFile!!.mkdirs(); writeBytes(bytes) }
    private fun staging(home: File) = File(home, "ocr-workspace/import-staging/import-${UUID.randomUUID()}").apply { mkdirs() }
    private fun rejects(expected: String? = null, action: () -> Unit) {
        try { action(); fail("The entire import must fail") } catch (error: Exception) {
            if (expected != null) assertTrue("Unexpected error: ${error.message}", error.message.orEmpty().contains(expected, true))
        }
    }

    @Test fun pinnedFixturesMatchTheirDeclaredSources() {
        val metadata = manifest(); val pdf = fixture("two-pages.pdf")
        assertEquals(metadata.getString("sha256"), digest(pdf)); assertEquals(metadata.getInt("bytes"), pdf.size)
        for (group in listOf("images", "pdf_variants")) {
            val files = metadata.getJSONArray(group)
            for (index in 0 until files.length()) {
                val entry = files.getJSONObject(index); val bytes = fixture(entry.getString("fixture"))
                assertEquals(entry.getString("sha256"), digest(bytes)); assertEquals(entry.getInt("bytes"), bytes.size)
            }
        }
    }

    @Test fun rendersEveryPdfPageWithInkPhysicalSizeAndUnchangedOriginal() = isolated { wrapped, home ->
        val original = fixture("two-pages.pdf"); val source = archive(home, original, "pdf"); val output = staging(home)
        val result = DocumentRasterizer(wrapped).rasterize(source.path, output.path, "pdf")
        val pages = result.getJSONArray("pages"); assertEquals(2, pages.length())
        val sizes = listOf(Pair(1700, 2200), Pair(1200, 1800)); val physical = listOf(Pair(612.0, 792.0), Pair(432.0, 648.0))
        var bytes = 0L
        for (index in 0 until pages.length()) {
            val page = pages.getJSONObject(index); val path = File(page.getString("path")); bytes += path.length()
            assertEquals(output.canonicalFile, path.canonicalFile.parentFile)
            assertEquals(physical[index].first, page.getDouble("width_points"), 0.001)
            assertEquals(physical[index].second, page.getDouble("height_points"), 0.001)
            val raster = BitmapFactory.decodeFile(path.path); assertNotNull(raster)
            try {
                assertEquals(sizes[index].first, raster.width); assertEquals(sizes[index].second, raster.height)
                var ink = 0
                for (y in 0 until raster.height step 8) for (x in 0 until raster.width step 8) {
                    if (Color.red(raster.getPixel(x, y)) < 120) ink++
                }
                assertTrue("Rendered text must not be a blank success", ink > 30)
                assertEquals(Color.WHITE, raster.getPixel(raster.width - 2, raster.height - 2))
            } finally { raster.recycle() }
        }
        assertEquals(bytes, result.getLong("bytes")); assertArrayEquals(original, source.readBytes())
        assertEquals(2, JSONObject(File(output, "progress.json").readText()).getInt("completed"))
    }

    @Test fun respectsPdfRotationAndFailsLockedOrDamagedDocuments() = isolated { wrapped, home ->
        val source = archive(home, fixture("rotated.pdf.b64"), "pdf")
        val first = DocumentRasterizer(wrapped).rasterize(source.path, staging(home).path, "pdf").getJSONArray("pages").getJSONObject(0)
        assertEquals(792.0, first.getDouble("width_points"), 0.001); assertEquals(612.0, first.getDouble("height_points"), 0.001)
        val locked = archive(home, fixture("locked.pdf.b64"), "pdf")
        rejects("password protected") { DocumentRasterizer(wrapped).rasterize(locked.path, staging(home).path, "pdf") }
        val damaged = archive(home, "%PDF-1.7 broken input".toByteArray(), "pdf")
        rejects("PDF") { DocumentRasterizer(wrapped).rasterize(damaged.path, staging(home).path, "pdf") }
    }

    @Test fun decodesOrientationInPixelsBeforeRecognition() = isolated { wrapped, home ->
        assumeTrue(Build.VERSION.SDK_INT >= 28)
        val original = fixture("oriented.jpg.b64"); val source = archive(home, original, "jpg")
        val page = DocumentRasterizer(wrapped).rasterize(source.path, staging(home).path, "image").getJSONArray("pages").getJSONObject(0)
        assertEquals(80, page.getInt("width")); assertEquals(120, page.getInt("height"))
        val image = BitmapFactory.decodeFile(page.getString("path"))
        try { val topLeft = image.getPixel(10, 10); assertTrue(Color.blue(topLeft) > 180 && Color.red(topLeft) < 70) }
        finally { image.recycle() }
        assertArrayEquals(original, source.readBytes())
    }

    @Test fun decodesAvifOnlyWhenTheRuntimeActuallySupportsIt() = isolated { wrapped, home ->
        val formats = DocumentRasterizer.capabilities().getJSONArray("image_formats")
        assumeTrue((0 until formats.length()).any { formats.getString(it) == "avif" })
        val original = fixture("colors.avif.b64"); val source = archive(home, original, "avif")
        val page = DocumentRasterizer(wrapped).rasterize(source.path, staging(home).path, "image").getJSONArray("pages").getJSONObject(0)
        assertEquals(120, page.getInt("width")); assertEquals(80, page.getInt("height"))
        val image = BitmapFactory.decodeFile(page.getString("path"))
        try { val topLeft = image.getPixel(10, 10); assertTrue(Color.red(topLeft) > 180 && Color.blue(topLeft) < 70) }
        finally { image.recycle() }
        assertArrayEquals(original, source.readBytes())
    }

    @Test fun rejectsIncompleteImagePayloadsInsteadOfImportingPartialPixels() = isolated { wrapped, home ->
        assumeTrue(Build.VERSION.SDK_INT >= 28)
        val complete = fixture("oriented.jpg.b64")
        val source = archive(home, complete.copyOf(complete.size - 100), "jpg")
        rejects { DocumentRasterizer(wrapped).rasterize(source.path, staging(home).path, "image") }
    }

    @Test fun cancelsBeforeOpeningAndBetweenPagesWithoutReturningAPartialDocument() = isolated { wrapped, home ->
        val original = fixture("two-pages.pdf"); val source = archive(home, original, "pdf"); val cancelled = staging(home)
        File(cancelled, "cancel").writeText("")
        rejects("OCR_IMPORT_CANCELLED") { DocumentRasterizer(wrapped).rasterize(source.path, cancelled.path, "pdf") }
        assertEquals(0, cancelled.listFiles()!!.count { it.extension == "png" })
        val output = staging(home)
        val decoder = DocumentRasterizer(wrapped) { completed, _ -> if (completed == 1) File(output, "cancel").writeText("") }
        rejects("OCR_IMPORT_CANCELLED") { decoder.rasterize(source.path, output.path, "pdf") }
        assertEquals(1, output.listFiles()!!.count { it.extension == "png" }); assertArrayEquals(original, source.readBytes())
    }

    @Test fun rejectsOutOfBoundsSourcesSymlinksAndReusedStaging() = isolated { wrapped, home ->
        val original = fixture("two-pages.pdf"); val source = archive(home, original, "pdf"); val decoder = DocumentRasterizer(wrapped)
        val outside = File(home, "outside.pdf").apply { writeBytes(original) }
        rejects { decoder.rasterize(outside.path, staging(home).path, "pdf") }
        val alias = File(source.parentFile, "f".repeat(64) + ".pdf"); Os.symlink(outside.path, alias.path)
        rejects { decoder.rasterize(alias.path, staging(home).path, "pdf") }
        val reused = staging(home); File(reused, "unrelated.txt").writeText("keep me")
        rejects { decoder.rasterize(source.path, reused.path, "pdf") }; assertTrue(File(reused, "unrelated.txt").exists())
        RandomAccessFile(source, "rw").use { it.setLength(DocumentRasterizer.MAX_SOURCE_BYTES + 1) }
        rejects("32 MiB") { decoder.rasterize(source.path, staging(home).path, "pdf") }
    }

    @Test fun capsRasterMemoryWithoutOverflowOrDegenerateDimensions() {
        for ((width, height) in listOf(Pair(14400.0, 14400.0), Pair(100000.0, 1.0), Pair(10000.0, 10000.0))) {
            val size = DocumentRasterizer.boundedSize(width, height, 200.0 / 72)
            assertTrue(size.first > 0 && size.second > 0 && size.first.toLong() * size.second <= DocumentRasterizer.MAX_PAGE_PIXELS)
        }
        rejects { DocumentRasterizer.boundedSize(Double.NaN, 42.0, 1.0) }
    }
}
