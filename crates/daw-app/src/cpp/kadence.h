#pragma once

#include "rust/cxx.h"

namespace kadence {

// Creates the QApplication, sets up KAboutData, KLocalizedString, KCrash and
// the QML engine, loads Main.qml and runs the event loop.
//
// It is a QApplication, not a QGuiApplication, because the KDE desktop
// QtQuick style (qqc2-desktop-style) draws through QStyle.
// `args` is each raw argument followed by a NUL byte.
int runApplication(rust::Slice<const uint8_t> args, rust::Str version);

// Saves the visible top-level window titled `title` as an image file.
bool saveWindowScreenshot(rust::Str title, rust::Str path);

rust::String configRead(rust::Str group, rust::Str key);
void configWrite(rust::Str group, rust::Str key, rust::Str value);
void configSync();

} // namespace kadence
