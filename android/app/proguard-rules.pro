# kotlinx.serialization, zxing and the AndroidX libraries bring their own rules; the manifest's classes and the layouts' views are kept by AAPT.
# Crash lines in the saved "Crashed on" notes and in logcat keep their file and line numbers.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
