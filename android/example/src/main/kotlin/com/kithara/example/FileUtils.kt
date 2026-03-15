package com.kithara.example

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal data class ImportedFile(
    val displayName: String,
    val path: String,
)

internal suspend fun copyDocumentToCache(context: Context, uri: Uri): ImportedFile =
    withContext(Dispatchers.IO) {
        val displayName = queryDisplayName(context, uri)
            ?.takeIf(String::isNotBlank)
            ?: uri.lastPathSegment?.substringAfterLast('/')
            ?: "selected-audio"
        val importDir = File(context.cacheDir, "imports")
        if (!importDir.exists() && !importDir.mkdirs()) {
            throw IOException("Unable to create import directory")
        }

        val outputFile = buildImportedFile(importDir, displayName)

        context.contentResolver.openInputStream(uri)?.use { input ->
            outputFile.outputStream().use { output -> input.copyTo(output) }
        } ?: throw IOException("Unable to open selected file")

        ImportedFile(
            displayName = displayName,
            path = outputFile.absolutePath,
        )
    }

internal fun buildImportedFile(
    importDir: File,
    fileName: String,
    id: UUID = UUID.randomUUID(),
): File = File(importDir, "$id-$fileName")

private fun queryDisplayName(context: Context, uri: Uri): String? =
    context.contentResolver.query(
        uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null,
    )?.use { cursor ->
        val col = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
        if (col < 0 || !cursor.moveToFirst()) null else cursor.getString(col)
    }
