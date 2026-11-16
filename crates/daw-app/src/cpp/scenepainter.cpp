// SPDX-License-Identifier: GPL-3.0-or-later
#include "daw-app/src/cpp/scenepainter.h"
#include "daw-app/src/paint.cxx.h"

#include <QCache>
#include <QFontMetricsF>
#include <QGuiApplication>
#include <QImage>
#include <QPainter>
#include <QPainterPath>
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

// Laid-out, elided labels, kept between frames: the same clip names are
// drawn every frame while the view scrolls, and laying text out costs far
// more than drawing it. Only the thread that paints uses it.
struct LabelKey {
    QByteArray text;
    int width;
    bool bold;
    bool operator==(const LabelKey &o) const
    {
        return width == o.width && bold == o.bold && text == o.text;
    }
};

size_t qHash(const LabelKey &k, size_t seed = 0)
{
    return qHashMulti(seed, k.text, k.width, k.bold);
}

struct Labels {
    QFont font;
    QCache<LabelKey, QStaticText> cache{2000};
};

const QStaticText &label(Labels &labels, const QFont &font, const QFontMetricsF &metrics,
                         const rust::String &s, qreal width, bool bold)
{
    // Whole pixels of width, so a label is laid out once per size rather
    // than once per position.
    const LabelKey key{QByteArray(s.data(), static_cast<qsizetype>(s.size())), qFloor(width), bold};
    if (QStaticText *hit = labels.cache.object(key)) {
        return *hit;
    }
    auto *made = new QStaticText(metrics.elidedText(QString::fromUtf8(key.text), Qt::ElideRight, key.width));
    made->setTextFormat(Qt::PlainText);
    made->prepare(QTransform(), font);
    // The cache owns it; it is not dropped before the next insert.
    labels.cache.insert(key, made);
    return *made;
}

// A box drawn whole, antialiased: the reference the fast path below must
// match.
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
    int scale;
    bool operator==(const CornerKey &o) const
    {
        return fill == o.fill && stroke == o.stroke && width == o.width && radius == o.radius && scale == o.scale;
    }
};

size_t qHash(const CornerKey &k, size_t seed = 0)
{
    return qHashMulti(seed, k.fill, k.stroke, k.width, k.radius, k.scale);
}

using Corners = QCache<CornerKey, QImage>;

// Draws a box from cached corners and plain rectangles, on whole pixels.
// False, with nothing drawn, where that would not look the same as
// `drawBox`: a painter that is not on a whole-number pixel grid, a
// see-through or fractional outline, or a box too small to have four
// separate corners.
bool drawBoxFast(QPainter &p, Corners &corners, const QRectF &r, const PaintOp &op)
{
    const QTransform &t = p.deviceTransform();
    const int scale = qRound(t.m11());
    const auto whole = [](qreal v) { return v == std::round(v); };
    if (t.type() > QTransform::TxScale || t.m11() != t.m22() || scale < 1 || !whole(t.m11()) || !whole(t.dx())
        || !whole(t.dy())) {
        return false;
    }
    if (!whole(op.width) || op.width < 0 || (op.width > 0 && qAlpha(op.stroke) != 255)) {
        return false;
    }
    const int border = static_cast<int>(op.width);
    // How far a corner reaches along each edge.
    const int c = std::max(qCeil(op.radius + op.width / 2.0), border);
    const QRect box = r.toRect();
    if (c == 0 || box.width() < 2 * c || box.height() < 2 * c) {
        return false;
    }

    const CornerKey key{op.fill, op.stroke, op.width, op.radius, scale};
    QImage *image = corners.object(key);
    if (!image) {
        image = new QImage(QSize(2 * c, 2 * c) * scale, QImage::Format_ARGB32_Premultiplied);
        image->setDevicePixelRatio(scale);
        image->fill(Qt::transparent);
        QPainter corner(image);
        drawBox(corner, QRectF(0, 0, 2 * c, 2 * c), op);
        corner.end();
        // The cache owns it; it is not dropped before the next insert.
        corners.insert(key, image);
    }

    const int x = box.x(), y = box.y(), w = box.width(), h = box.height();
    const int far = c * scale;
    const auto corner = [&](int dx, int dy, int sx, int sy) {
        p.drawImage(QRectF(x + dx, y + dy, c, c), *image, QRectF(sx, sy, far, far));
    };
    corner(0, 0, 0, 0);
    corner(w - c, 0, far, 0);
    corner(0, h - c, 0, far);
    corner(w - c, h - c, far, far);

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
    const QFont regular = QGuiApplication::font();
    QFont bold = regular;
    bold.setBold(true);
    const QFontMetricsF regularMetrics(regular);
    const QFontMetricsF boldMetrics(bold);

    static thread_local Corners corners{256};
    static thread_local Labels labels;
    if (labels.font != regular) {
        labels.cache.clear();
        labels.font = regular;
    }

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
            if (op.x == op.w || op.y == op.h) {
                const qreal half = op.width / 2.0;
                p.fillRect(QRectF(QPointF(op.x, op.y), QPointF(op.w, op.h)).normalized().adjusted(
                               op.x == op.w ? -half : 0, op.y == op.h ? -half : 0,
                               op.x == op.w ? half : 0, op.y == op.h ? half : 0),
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
            const auto &metrics = isBold ? boldMetrics : regularMetrics;
            const QStaticText &text = label(labels, isBold ? bold : regular, metrics, texts[op.text], op.w, isBold);
            p.setFont(isBold ? bold : regular);
            p.setPen(color(op.fill));
            // Centred vertically, and horizontally if asked.
            const QSizeF size = text.size();
            const qreal x = (op.flags & 2) ? op.x + (op.w - size.width()) / 2.0 : op.x;
            p.drawStaticText(QPointF(x, std::round(op.y + (op.h - size.height()) / 2.0)), text);
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
            // At least a pixel tall, so silence still shows a line.
            const auto bottom = [&](size_t i) { return std::max(mid - waves[start + i * 2] * half, top(i) + 1.0); };
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
