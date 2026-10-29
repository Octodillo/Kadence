// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include "rust/cxx.h"

class QPainter;

namespace kadence {

struct PaintOp;

// Paints a flattened daw_arrange::Scene (see items/paint.rs).
void paintScene(QPainter &painter,
                rust::Slice<const PaintOp> ops,
                rust::Slice<const rust::String> texts,
                rust::Slice<const float> waves);

} // namespace kadence
