# UniFFI's generated Kotlin uses JNA, which reflects over the mapped interfaces
# and callback structs. Anything R8 renames there breaks the native call at
# runtime, not at build time, so keep it all.
-keep class com.sun.jna.** { *; }
-keep interface com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { *; }
-keep class * implements com.sun.jna.Library { *; }
-keep class * implements com.sun.jna.Callback { *; }

# JNA's Native$AWT is a desktop-only helper (java.awt.Component/Window/etc. for
# embedding native windows in a Swing/AWT app) that's never reachable on
# Android - there is no java.awt here. R8 can't prove that statically, so it
# hard-errors on the missing classes unless told these are expected.
-dontwarn java.awt.**

# The generated bindings and the callback interfaces the Rust side invokes.
-keep class dev.termland.core.** { *; }
-keep interface dev.termland.core.** { *; }

# Kotlin metadata JNA/UniFFI structure layout depends on field order.
-keepclassmembers class dev.termland.core.** { <fields>; }

# kotlinx.serialization keeps its generated serializers on the companion.
-keepclassmembers class dev.termland.android.data.** {
    *** Companion;
}
-keepclasseswithmembers class dev.termland.android.data.** {
    kotlinx.serialization.KSerializer serializer(...);
}
