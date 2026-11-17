// SPDX-License-Identifier: GPL-3.0-or-later
#include "daw-app/src/cpp/scenepainter.h"
#include "daw-app/src/paint.cxx.h"

#include <QCache>
#include <QFontMetricsF>
#include <QGuiApplication>
#include <QImage>
#include <QPainter>
#include <QPolygonF>
#include <QStaticText>
#include <QtMath>

#include <algorithm>
#include <cmath>

namespace kadence {

namespace {

constexpr quint8 Rect = 0;
constexpr quint8 RoundRect = 1;
constexpr quint8 Line = 2;
constexpr quint8 Text = 3;
constexpr quint8 Wave = 4;
constexpr quint8 PushClip = 5;
constexpr quint8 PopClip = 6;

QColor color(quint32 argb)
{
    return QColor::fromRgba(argb);
}

QRectF rectOf(const PaintOp &op)
{
    return QRectF(op.x, op.y, op.w, op.h);
}

// A font with its measurements.
struct Face {
    QFont font;
    QFontMetricsF metrics;
    explicit Face(const QFont &f)
        : font(f)
        , metrics(f)
    {
    }
};

// Laid-out labels, kept between frames: the same clip names are drawn
// every frame while the view scrolls, and laying text out costs far more
// than drawing it. Only the thread that paints uses it.
//
// Both caches below own what they hold and drop entries only on insert, so
// what a lookup returns is good until the next one.
struct LabelKey {
    QByteArray text;
    // The whole pixels the text was elided to, or `Whole` for all of it.
    int width;
    bool bold;
    bool operator==(const LabelKey &o) const
    {
        return width == o.width && bold == o.bold && text == o.text;
    }
};

constexpr int Whole = -1;

size_t qHash(const LabelKey &k, size_t seed = 0)
{
    return qHashMulti(seed, k.text, k.width, k.bold);
}

struct Label {
    QStaticText text;
    qreal width;
};

struct Labels {
    QFont font;
    QCache<LabelKey, Label> cache{4000};
};

// The label for `s` in at most `width`: the whole text where it fits, which
// is one entry however the view is scrolled or zoomed, and otherwise the
// text elided to that many whole pixels.
const Label &label(Labels &labels, const Face &face, bool bold, const QTransform &transform,
                   const rust::String &s, qreal width)
{
    const auto find = [&](LabelKey key, const auto &text) -> const Label & {
        if (const Label *hit = labels.cache.object(key)) {
            return *hit;
        }
        auto *made = new Label{QStaticText(text()), 0.0};
        made->text.setTextFormat(Qt::PlainText);
        made->text.prepare(transform, face.font);
        made->width = made->text.size().width();
        labels.cache.insert(key, made);
        return *made;
    };
    QByteArray utf8(s.data(), static_cast<qsizetype>(s.size()));
    const auto whole = [&] { return QString::fromUtf8(utf8); };
    const Label &all = find({utf8, Whole, bold}, whole);
    if (all.width <= width) {
        return all;
    }
    const int pixels = qFloor(std::min(width, 1e6));
    return find({std::move(utf8), pixels, bold},
                [&] { return face.metrics.elidedText(whole(), Qt::ElideRight, pixels); });
}

// A box drawn whole, antialiased: what the fast path below stands in for.
void drawBox(QPainter &p, const QRectF &r, const PaintOp &op)
{
    const qreal half = op.width / 2.0;
    QPen pen(color(op.stroke));
    pen.setWidthF(op.width);
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(op.width > 0 ? pen : QPen(Qt::NoPen));
    p.setBrush(color(op.fill));
    p.drawRoundedRect(r.adjusted(half, half, -half, -half), op.radius, op.radius);
    p.setRenderHint(QPainter::Antialiasing, false);
}

// The corners of boxes of one look, drawn once as a small box and copied
// from there: only the corners of a box need antialiasing, and filling a
// whole clip through the antialiasing rasteriser for their sake is most of
// what a box costs.
struct CornerKey {
    quint32 fill;
    quint32 stroke;
    qreal width;
    qreal radius;
    qreal scale;
    bool operator==(const CornerKey &o) const
    {
        return fill == o.fill && stroke == o.stroke && width == o.width && radius == o.radius && scale == o.scale;
    }
};

size_t qHash(const CornerKey &k, size_t seed = 0)
{
    return qHashMulti(seed, k.fill, k.stroke, k.width, k.radius, k.scale);
}

// Small images, and a piano roll has a look for each velocity.
using Corners = QCache<CornerKey, QImage>;
constexpr int CornerLooks = 2048;

// Draws a box from cached corners and plain rectangles. It works in device
// pixels, each edge of the box rounded to the nearest as a plain rectangle's
// are, so a box lines up with what is drawn inside and beside it. False,
// with nothing drawn, where a box cannot be put together this way: a
// painter that does more than scale evenly, an outline that is see-through
// or not a whole number of device pixels, or a box too small to have four
// separate corners.
bool drawBoxFast(QPainter &p, Corners &corners, const QRectF &r, const PaintOp &op)
{
    const QTransform &t = p.deviceTransform();
    const qreal scale = t.m11();
    if (t.type() > QTransform::TxScale || scale != t.m22() || scale <= 0 || t != p.worldTransform()) {
        return false;
    }
    const qreal outline = op.width * scale;
    if (outline < 0 || outline != std::round(outline) || (outline > 0 && qAlpha(op.stroke) != 255)) {
        return false;
    }
    const int border = static_cast<int>(outline);
    // How far a corner reaches along each edge.
    const int c = std::max(qCeil((op.radius + op.width / 2.0) * scale), border);
    const QRectF device = t.mapRect(r);
    const int x = qRound(device.left()), y = qRound(device.top());
    const int w = qRound(device.right()) - x, h = qRound(device.bottom()) - y;
    if (c == 0 || w < 2 * c || h < 2 * c) {
        return false;
    }

    // Without an outline its colour is not part of the look.
    const CornerKey key{op.fill, border > 0 ? op.stroke : 0, op.width, op.radius, scale};
    QImage *image = corners.object(key);
    if (!image) {
        image = new QImage(2 * c, 2 * c, QImage::Format_ARGB32_Premultiplied);
        image->fill(Qt::transparent);
        QPainter corner(image);
        corner.scale(scale, scale);
        drawBox(corner, QRectF(0, 0, 2 * c / scale, 2 * c / scale), op);
        corner.end();
        corners.insert(key, image);
    }

    p.setWorldMatrixEnabled(false);
    const auto corner = [&](int dx, int dy) {
        p.drawImage(QPoint(x + (dx ? w - c : 0), y + (dy ? h - c : 0)), *image, QRect(dx * c, dy * c, c, c));
    };
    corner(0, 0);
    corner(1, 0);
    corner(0, 1);
    corner(1, 1);

    const QColor fill = color(op.fill);
    p.fillRect(x + c, y + border, w - 2 * c, h - 2 * border, fill);
    p.fillRect(x + border, y + c, c - border, h - 2 * c, fill);
    p.fillRect(x + w - c, y + c, c - border, h - 2 * c, fill);
    if (border > 0) {
        const QColor stroke = color(op.stroke);
        p.fillRect(x + c, y, w - 2 * c, border, stroke);
        p.fillRect(x + c, y + h - border, w - 2 * c, border, stroke);
        p.fillRect(x, y + c, border, h - 2 * c, stroke);
        p.fillRect(x + w - border, y + c, border, h - 2 * c, stroke);
    }
    p.setWorldMatrixEnabled(true);
    return true;
}

} // namespace

void paintScene(QPainter &p,
                rust::Slice<const PaintOp> ops,
                rust::Slice<const rust::String> texts,
                rust::Slice<const float> waves)
{
    // The application font, as the rest of the UI uses (Kirigami's
    // defaultFont); bold for emphasis.
    QFont boldFont = QGuiApplication::font();
    boldFont.setBold(true);
    const Face regular(QGuiApplication::font());
    const Face bold(boldFont);

    static thread_local Corners corners{CornerLooks};
    static thread_local Labels labels;
    if (labels.font != regular.font) {
        labels.cache.clear();
        labels.font = regular.font;
    }
    // Device pixels to each of the painter's.
    const qreal scale = p.deviceTransform().m22();

    p.setRenderHint(QPainter::Antialiasing, false);
    QPolygonF wave;
    for (const PaintOp &op : ops) {
        switch (op.kind) {
        case Rect:
            p.fillRect(rectOf(op), color(op.fill));
            break;
        case RoundRect:
            if (!drawBoxFast(p, corners, rectOf(op), op)) {
                drawBox(p, rectOf(op), op);
            }
            break;
        case Line: {
            // Grid lines and the like run along the pixel grid, where a
            // rectangle is the same line without the cost of stroking it.
            const bool vertical = op.x == op.w;
            const bool horizontal = op.y == op.h;
            if (vertical != horizontal && op.width > 0) {
                const qreal dx = vertical ? op.width / 2.0 : 0.0;
                const qreal dy = horizontal ? op.width / 2.0 : 0.0;
                p.fillRect(QRectF(QPointF(op.x, op.y), QPointF(op.w, op.h)).normalized().adjusted(-dx, -dy, dx, dy),
                           color(op.stroke));
                break;
            }
            QPen pen(color(op.stroke));
            pen.setWidthF(op.width);
            pen.setCapStyle(Qt::FlatCap);
            p.setPen(pen);
            p.drawLine(QLineF(op.x, op.y, op.w, op.h));
            break;
        }
        case Text: {
            if (op.text >= texts.size() || op.w <= 0) {
                break;
            }
            const bool isBold = op.flags & 1;
            const Face &face = isBold ? bold : regular;
            const Label &made = label(labels, face, isBold, p.transform(), texts[op.text], op.w);
            const QSizeF size = made.text.size();
            // Centred vertically, on a whole device pixel, and horizontally
            // if asked.
            const qreal x = (op.flags & 2) ? op.x + (op.w - size.width()) / 2.0 : op.x;
            const qreal y = std::round((op.y + (op.h - size.height()) / 2.0) * scale) / scale;
            // A font taller than its row is cut off at the row.
            const bool tall = size.height() > op.h;
            if (tall) {
                p.save();
                p.setClipRect(rectOf(op), Qt::IntersectClip);
            }
            p.setFont(face.font);
            p.setPen(color(op.fill));
            p.drawStaticText(QPointF(x, y), made.text);
            if (tall) {
                p.restore();
            }
            break;
        }
        case Wave: {
            const size_t start = op.wave_start * 2;
            const size_t len = op.wave_len;
            if (len == 0 || start + len * 2 > waves.size()) {
                break;
            }
            // Top edge left to right through the maxima, back along the
            // minima, filled. Each column's points sit on its pixel centre
            // and the fill is not antialiased, which costs half as much
            // and paints each column from its maximum to its minimum
            // exactly.
            const qreal mid = op.y + op.h / 2.0;
            const qreal half = op.h / 2.0 * 0.92;
            const auto top = [&](size_t i) { return mid - waves[start + i * 2 + 1] * half; };
            // Down to the middle at least, and a pixel tall, so silence
            // still shows a line.
            const auto bottom = [&](size_t i) {
                return std::max({mid - waves[start + i * 2] * half, mid + 0.5, top(i) + 1.0});
            };
            wave.resize(0);
            wave.reserve(static_cast<qsizetype>(len * 2 + 4));
            wave << QPointF(op.x, top(0));
            for (size_t i = 0; i < len; ++i) {
                wave << QPointF(op.x + i + 0.5, top(i));
            }
            wave << QPointF(op.x + len, top(len - 1)) << QPointF(op.x + len, bottom(len - 1));
            for (size_t i = len; i-- > 0;) {
                wave << QPointF(op.x + i + 0.5, bottom(i));
            }
            wave << QPointF(op.x, bottom(0));
            p.setPen(Qt::NoPen);
            p.setBrush(color(op.fill));
            p.drawPolygon(wave);
            break;
        }
        case PushClip:
            p.save();
            p.setClipRect(rectOf(op), Qt::IntersectClip);
            break;
        case PopClip:
            p.restore();
            break;
        default:
            break;
        }
    }
}

} // namespace kadence
