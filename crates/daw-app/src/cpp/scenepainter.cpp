// SPDX-License-Identifier: GPL-3.0-or-later
#include "daw-app/src/cpp/scenepainter.h"
#include "daw-app/src/items/paint.cxx.h"

#include <QFontMetricsF>
#include <QGuiApplication>
#include <QPainter>
#include <QPainterPath>
#include <QPolygonF>

#include <algorithm>

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

    p.setRenderHint(QPainter::Antialiasing, false);
    for (const PaintOp &op : ops) {
        switch (op.kind) {
        case Rect:
            p.fillRect(rectOf(op), color(op.fill));
            break;
        case RoundRect: {
            p.setRenderHint(QPainter::Antialiasing, true);
            const qreal half = op.width / 2.0;
            QPen pen(color(op.stroke));
            pen.setWidthF(op.width);
            p.setPen(op.width > 0 ? pen : QPen(Qt::NoPen));
            p.setBrush(color(op.fill));
            p.drawRoundedRect(rectOf(op).adjusted(half, half, -half, -half), op.radius, op.radius);
            p.setRenderHint(QPainter::Antialiasing, false);
            break;
        }
        case Line: {
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
            const auto &s = texts[op.text];
            const QString text = QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size()));
            p.setFont(isBold ? bold : regular);
            p.setPen(color(op.fill));
            const Qt::Alignment align = Qt::AlignVCenter | ((op.flags & 2) ? Qt::AlignHCenter : Qt::AlignLeft);
            p.drawText(rectOf(op), align, metrics.elidedText(text, Qt::ElideRight, op.w));
            break;
        }
        case Wave: {
            const size_t start = op.wave_start * 2;
            const size_t len = op.wave_len;
            if (len == 0 || start + len * 2 > waves.size()) {
                break;
            }
            // Top edge left to right through the maxima, back along the
            // minima, filled.
            const qreal mid = op.y + op.h / 2.0;
            const qreal half = op.h / 2.0 * 0.92;
            QPolygonF poly;
            poly.reserve(static_cast<qsizetype>(len * 2));
            for (size_t i = 0; i < len; ++i) {
                poly << QPointF(op.x + i, mid - waves[start + i * 2 + 1] * half);
            }
            for (size_t i = len; i-- > 0;) {
                // At least a pixel tall, so silence still shows a line.
                const qreal lo = mid - waves[start + i * 2] * half;
                poly << QPointF(op.x + i + 1, std::max(lo, mid + 0.5));
            }
            p.setPen(Qt::NoPen);
            p.setBrush(color(op.fill));
            p.setRenderHint(QPainter::Antialiasing, true);
            p.drawPolygon(poly);
            p.setRenderHint(QPainter::Antialiasing, false);
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
