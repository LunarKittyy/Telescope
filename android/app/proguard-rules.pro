# kotlinx.serialization, zxing and the AndroidX libraries bring their own rules; the manifest's classes and the layouts' views are kept by AAPT.
# CI keeps each published build's mapping.txt as the android-mapping-<build> artifact, to read a trace from a shipped APK.
# Crash lines in the saved "Crashed on" notes and in logcat keep their file and line numbers.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
