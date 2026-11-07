#!/bin/bash
# Builds the test programs in java/ into DEX files, and writes down what a
# real JVM prints for them (the interpreter's tests compare with that).
#
# Needs a JDK (JAVA_HOME), dx (DX_JAR) and the Android API (ANDROID_JAR);
# the defaults are the Ubuntu packages openjdk-17-jdk-headless,
# dalvik-exchange and libandroid-23-java.
set -e
cd "$(dirname "$0")"
ROOT=${PKG_ROOT:-}
JAVA_HOME=${JAVA_HOME:-$ROOT/usr/lib/jvm/java-17-openjdk-amd64}
DX_JAR=${DX_JAR:-$ROOT/usr/share/java/com.android.dx.jar}
ANDROID_JAR=${ANDROID_JAR:-$ROOT/usr/share/java/com.android.android-23.jar}
JAVAC="$JAVA_HOME/bin/javac"
JAVA="$JAVA_HOME/bin/java"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

dex() { # dex <name> <classes dir>
    "$JAVA" -cp "$DX_JAR" com.android.dx.command.Main --dex --output="$1.dex" "$2"
}

for prog in Basics Input; do
    mkdir -p "$OUT/$prog"
    "$JAVAC" -nowarn --release 8 -encoding UTF-8 -d "$OUT/$prog" "java/$prog.java" 2>&1 | grep -v '^warning\|^1 warning' || true
    dex "$prog" "$OUT/$prog"
done
"$JAVA" -cp "$OUT/Basics" Basics > Basics.out
printf 'Mattia\n3\n4 5\n6\n2.5\nciao 7\nprima riga\n\nultima\n' > Input.in
"$JAVA" -cp "$OUT/Input" Input < Input.in > Input.out || echo "exit $?" >> Input.out

mkdir -p "$OUT/app/src" "$OUT/app/classes"
cp java/Counter.java "$OUT/app/src/MainActivity.java"
"$JAVAC" -nowarn --release 8 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$OUT/app/classes" "$OUT/app/src/MainActivity.java" 2>&1 | grep -v '^warning\|^1 warning' || true
dex Counter "$OUT/app/classes"

# An app that tests itself, packed as an .apk (a zip with classes.dex).
mkdir -p "$OUT/self"
"$JAVAC" -nowarn --release 8 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$OUT/self" java/SelfTest.java 2>&1 | grep -v '^warning\|^1 warning' || true
dex "$OUT/classes" "$OUT/self"
python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED); z.write(sys.argv[2], "classes.dex"); z.close()' SelfTest.apk "$OUT/classes.dex"
ls -l ./*.dex ./*.apk
