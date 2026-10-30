#include "daw-app/src/cpp/kadence.h"

#include <QApplication>
#include <QByteArray>
#include <QCommandLineParser>
#include <QDir>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QQuickStyle>
#include <QStandardPaths>
#include <QString>

#include <KAboutData>
#include <KColorScheme>
#include <KConfigGroup>
#include <KCrash>
#include <KIconTheme>
#include <KLocalizedQmlContext>
#include <KLocalizedString>
#include <KSharedConfig>

#include <vector>

namespace kadence {

namespace {

QString toQString(rust::Str s)
{
    return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size()));
}

// Outside Plasma no platform theme applies a KDE color scheme, so honour
// --color-scheme (a scheme name such as "BreezeDark") ourselves.
void applyColorScheme(const QString &name)
{
    const QString path = QStandardPaths::locate(QStandardPaths::GenericDataLocation,
                                                QStringLiteral("color-schemes/%1.colors").arg(name));
    if (path.isEmpty()) {
        qWarning("color scheme %s not found", qPrintable(name));
        return;
    }
    // The same two steps KColorSchemeManager takes: the palette for QStyle,
    // and the path that qqc2-desktop-style reads Kirigami.Theme colors from.
    qApp->setProperty("KDE_COLOR_SCHEME_PATH", path);
    QApplication::setPalette(KColorScheme::createApplicationPalette(KSharedConfig::openConfig(path)));
}

} // namespace

int runApplication(rust::Slice<const uint8_t> args, rust::Str version)
{
    // QApplication keeps references to argc and argv for its whole life.
    std::vector<QByteArray> storage;
    const auto *bytes = reinterpret_cast<const char *>(args.data());
    for (std::size_t start = 0, i = 0; i < args.size(); ++i) {
        if (bytes[i] == '\0') {
            storage.emplace_back(bytes + start, static_cast<qsizetype>(i - start));
            start = i + 1;
        }
    }
    std::vector<char *> argv;
    for (auto &arg : storage) {
        argv.push_back(arg.data());
    }
    int argc = static_cast<int>(argv.size());
    argv.push_back(nullptr);

    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE")) {
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
    }
    QQuickStyle::setFallbackStyle(QStringLiteral("Fusion"));

    // Outside Plasma, still use Breeze icons recolored to match the palette.
    KIconTheme::initTheme();
    QApplication app(argc, argv.data());
    // Plasma's platform theme picks the widget style; elsewhere default to
    // Breeze so qqc2-desktop-style controls look the same.
    if (!qEnvironmentVariable("XDG_CURRENT_DESKTOP").contains(QLatin1String("KDE"), Qt::CaseInsensitive)
        && qEnvironmentVariableIsEmpty("QT_STYLE_OVERRIDE")) {
        QApplication::setStyle(QStringLiteral("breeze"));
    }
    KLocalizedString::setApplicationDomain("kadence");

    KAboutData about(QStringLiteral("kadence"),
                     i18nc("@title", "Kadence"),
                     toQString(version),
                     i18n("Make music on the Plasma desktop"),
                     KAboutLicense::GPL_V3,
                     i18n("© 2026 The Kadence developers"));
    about.setLicense(KAboutLicense::GPL_V3, KAboutLicense::OrLaterVersions);
    about.setHomepage(QStringLiteral("https://github.com/Octodillo/daw"));
    about.setBugAddress(QByteArrayLiteral("https://github.com/Octodillo/daw/issues"));
    about.setOrganizationDomain(QByteArrayLiteral("octodillo.github.io"));
    about.setDesktopFileName(QStringLiteral("io.github.octodillo.daw"));
    about.addAuthor(i18n("The Kadence developers"), QString(), QString(),
                    QStringLiteral("https://github.com/Octodillo/daw"));
    about.setTranslator(i18nc("NAME OF TRANSLATORS", "Your names"),
                        i18nc("EMAIL OF TRANSLATORS", "Your emails"));
    KAboutData::setApplicationData(about);
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("io.github.octodillo.daw"),
                                                 QIcon::fromTheme(QStringLiteral("audio-x-generic"))));

    KCrash::initialize();

    QCommandLineParser parser;
    about.setupCommandLine(&parser);
    QCommandLineOption colorSchemeOption(QStringLiteral("color-scheme"),
                                         i18n("Use this color scheme instead of the desktop's."),
                                         i18nc("@info:shell", "name"));
    QCommandLineOption smokeTestOption(QStringLiteral("smoke-test"));
    smokeTestOption.setFlags(QCommandLineOption::HiddenFromHelp);
    QCommandLineOption screenshotOption(QStringLiteral("screenshot-dir"));
    screenshotOption.setValueName(QStringLiteral("dir"));
    screenshotOption.setFlags(QCommandLineOption::HiddenFromHelp);
    parser.addOptions({colorSchemeOption, smokeTestOption, screenshotOption});
    parser.process(app);
    about.processCommandLine(&parser);

    if (parser.isSet(colorSchemeOption)) {
        applyColorScheme(parser.value(colorSchemeOption));
    }

    QQmlApplicationEngine engine;
    KLocalization::setupLocalizedContext(&engine);
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreationFailed, &app,
                     [] { QCoreApplication::exit(1); }, Qt::QueuedConnection);
    QString screenshotDir;
    if (parser.isSet(screenshotOption)) {
        screenshotDir = QDir(parser.value(screenshotOption)).absolutePath();
        QDir().mkpath(screenshotDir);
    }
    engine.setInitialProperties({
        {QStringLiteral("aboutData"), QVariant::fromValue(engine.toScriptValue(KAboutData::applicationData()))},
        {QStringLiteral("smokeTest"), parser.isSet(smokeTestOption)},
        {QStringLiteral("screenshotDir"), screenshotDir},
    });
    engine.loadFromModule("io.github.octodillo.daw", "Main");
    if (engine.rootObjects().isEmpty()) {
        return 1;
    }
    return QApplication::exec();
}

bool saveWindowScreenshot(rust::Str title, rust::Str path)
{
    const QString wanted = toQString(title);
    const auto windows = QGuiApplication::topLevelWindows();
    for (QWindow *window : windows) {
        auto *quickWindow = qobject_cast<QQuickWindow *>(window);
        if (quickWindow && quickWindow->isVisible() && quickWindow->title() == wanted) {
            return quickWindow->grabWindow().save(toQString(path));
        }
    }
    qWarning("no visible window titled %s", qPrintable(wanted));
    return false;
}

rust::String configRead(rust::Str group, rust::Str key)
{
    const KConfigGroup cg(KSharedConfig::openConfig(), toQString(group));
    return rust::String(cg.readEntry(toQString(key).toUtf8().constData(), QString()).toStdString());
}

void configWrite(rust::Str group, rust::Str key, rust::Str value)
{
    KConfigGroup cg(KSharedConfig::openConfig(), toQString(group));
    cg.writeEntry(toQString(key).toUtf8().constData(), toQString(value));
}

void configSync()
{
    KSharedConfig::openConfig()->sync();
}

} // namespace kadence
