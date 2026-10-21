// The table encodes left/right/up/down weights in two bits per arm:
// 0 absent, 1 light, 2 heavy, 3 double. Higher bits encode dash count,
// rounded corners and the two diagonal directions. Keep Unicode's mixed
// weights: replacing every junction with a generic cross loses information.
const uint box_shapes[128] = uint[](
    0x0005u, // U+2500 LIGHT HORIZONTAL
    0x000au, // U+2501 HEAVY HORIZONTAL
    0x0050u, // U+2502 LIGHT VERTICAL
    0x00a0u, // U+2503 HEAVY VERTICAL
    0x0305u, // U+2504 LIGHT TRIPLE DASH HORIZONTAL
    0x030au, // U+2505 HEAVY TRIPLE DASH HORIZONTAL
    0x0350u, // U+2506 LIGHT TRIPLE DASH VERTICAL
    0x03a0u, // U+2507 HEAVY TRIPLE DASH VERTICAL
    0x0405u, // U+2508 LIGHT QUADRUPLE DASH HORIZONTAL
    0x040au, // U+2509 HEAVY QUADRUPLE DASH HORIZONTAL
    0x0450u, // U+250A LIGHT QUADRUPLE DASH VERTICAL
    0x04a0u, // U+250B HEAVY QUADRUPLE DASH VERTICAL
    0x0044u, // U+250C LIGHT DOWN AND RIGHT
    0x0048u, // U+250D DOWN LIGHT AND RIGHT HEAVY
    0x0084u, // U+250E DOWN HEAVY AND RIGHT LIGHT
    0x0088u, // U+250F HEAVY DOWN AND RIGHT
    0x0041u, // U+2510 LIGHT DOWN AND LEFT
    0x0042u, // U+2511 DOWN LIGHT AND LEFT HEAVY
    0x0081u, // U+2512 DOWN HEAVY AND LEFT LIGHT
    0x0082u, // U+2513 HEAVY DOWN AND LEFT
    0x0014u, // U+2514 LIGHT UP AND RIGHT
    0x0018u, // U+2515 UP LIGHT AND RIGHT HEAVY
    0x0024u, // U+2516 UP HEAVY AND RIGHT LIGHT
    0x0028u, // U+2517 HEAVY UP AND RIGHT
    0x0011u, // U+2518 LIGHT UP AND LEFT
    0x0012u, // U+2519 UP LIGHT AND LEFT HEAVY
    0x0021u, // U+251A UP HEAVY AND LEFT LIGHT
    0x0022u, // U+251B HEAVY UP AND LEFT
    0x0054u, // U+251C LIGHT VERTICAL AND RIGHT
    0x0058u, // U+251D VERTICAL LIGHT AND RIGHT HEAVY
    0x0064u, // U+251E UP HEAVY AND RIGHT DOWN LIGHT
    0x0094u, // U+251F DOWN HEAVY AND RIGHT UP LIGHT
    0x00a4u, // U+2520 VERTICAL HEAVY AND RIGHT LIGHT
    0x0068u, // U+2521 DOWN LIGHT AND RIGHT UP HEAVY
    0x0098u, // U+2522 UP LIGHT AND RIGHT DOWN HEAVY
    0x00a8u, // U+2523 HEAVY VERTICAL AND RIGHT
    0x0051u, // U+2524 LIGHT VERTICAL AND LEFT
    0x0052u, // U+2525 VERTICAL LIGHT AND LEFT HEAVY
    0x0061u, // U+2526 UP HEAVY AND LEFT DOWN LIGHT
    0x0091u, // U+2527 DOWN HEAVY AND LEFT UP LIGHT
    0x00a1u, // U+2528 VERTICAL HEAVY AND LEFT LIGHT
    0x0062u, // U+2529 DOWN LIGHT AND LEFT UP HEAVY
    0x0092u, // U+252A UP LIGHT AND LEFT DOWN HEAVY
    0x00a2u, // U+252B HEAVY VERTICAL AND LEFT
    0x0045u, // U+252C LIGHT DOWN AND HORIZONTAL
    0x0046u, // U+252D LEFT HEAVY AND RIGHT DOWN LIGHT
    0x0049u, // U+252E RIGHT HEAVY AND LEFT DOWN LIGHT
    0x004au, // U+252F DOWN LIGHT AND HORIZONTAL HEAVY
    0x0085u, // U+2530 DOWN HEAVY AND HORIZONTAL LIGHT
    0x0086u, // U+2531 RIGHT LIGHT AND LEFT DOWN HEAVY
    0x0089u, // U+2532 LEFT LIGHT AND RIGHT DOWN HEAVY
    0x008au, // U+2533 HEAVY DOWN AND HORIZONTAL
    0x0015u, // U+2534 LIGHT UP AND HORIZONTAL
    0x0016u, // U+2535 LEFT HEAVY AND RIGHT UP LIGHT
    0x0019u, // U+2536 RIGHT HEAVY AND LEFT UP LIGHT
    0x001au, // U+2537 UP LIGHT AND HORIZONTAL HEAVY
    0x0025u, // U+2538 UP HEAVY AND HORIZONTAL LIGHT
    0x0026u, // U+2539 RIGHT LIGHT AND LEFT UP HEAVY
    0x0029u, // U+253A LEFT LIGHT AND RIGHT UP HEAVY
    0x002au, // U+253B HEAVY UP AND HORIZONTAL
    0x0055u, // U+253C LIGHT VERTICAL AND HORIZONTAL
    0x0056u, // U+253D LEFT HEAVY AND RIGHT VERTICAL LIGHT
    0x0059u, // U+253E RIGHT HEAVY AND LEFT VERTICAL LIGHT
    0x005au, // U+253F VERTICAL LIGHT AND HORIZONTAL HEAVY
    0x0065u, // U+2540 UP HEAVY AND DOWN HORIZONTAL LIGHT
    0x0095u, // U+2541 DOWN HEAVY AND UP HORIZONTAL LIGHT
    0x00a5u, // U+2542 VERTICAL HEAVY AND HORIZONTAL LIGHT
    0x0066u, // U+2543 LEFT UP HEAVY AND RIGHT DOWN LIGHT
    0x0069u, // U+2544 RIGHT UP HEAVY AND LEFT DOWN LIGHT
    0x0096u, // U+2545 LEFT DOWN HEAVY AND RIGHT UP LIGHT
    0x0099u, // U+2546 RIGHT DOWN HEAVY AND LEFT UP LIGHT
    0x006au, // U+2547 DOWN LIGHT AND UP HORIZONTAL HEAVY
    0x009au, // U+2548 UP LIGHT AND DOWN HORIZONTAL HEAVY
    0x00a6u, // U+2549 RIGHT LIGHT AND LEFT VERTICAL HEAVY
    0x00a9u, // U+254A LEFT LIGHT AND RIGHT VERTICAL HEAVY
    0x00aau, // U+254B HEAVY VERTICAL AND HORIZONTAL
    0x0205u, // U+254C LIGHT DOUBLE DASH HORIZONTAL
    0x020au, // U+254D HEAVY DOUBLE DASH HORIZONTAL
    0x0250u, // U+254E LIGHT DOUBLE DASH VERTICAL
    0x02a0u, // U+254F HEAVY DOUBLE DASH VERTICAL
    0x000fu, // U+2550 DOUBLE HORIZONTAL
    0x00f0u, // U+2551 DOUBLE VERTICAL
    0x004cu, // U+2552 DOWN SINGLE AND RIGHT DOUBLE
    0x00c4u, // U+2553 DOWN DOUBLE AND RIGHT SINGLE
    0x00ccu, // U+2554 DOUBLE DOWN AND RIGHT
    0x0043u, // U+2555 DOWN SINGLE AND LEFT DOUBLE
    0x00c1u, // U+2556 DOWN DOUBLE AND LEFT SINGLE
    0x00c3u, // U+2557 DOUBLE DOWN AND LEFT
    0x001cu, // U+2558 UP SINGLE AND RIGHT DOUBLE
    0x0034u, // U+2559 UP DOUBLE AND RIGHT SINGLE
    0x003cu, // U+255A DOUBLE UP AND RIGHT
    0x0013u, // U+255B UP SINGLE AND LEFT DOUBLE
    0x0031u, // U+255C UP DOUBLE AND LEFT SINGLE
    0x0033u, // U+255D DOUBLE UP AND LEFT
    0x005cu, // U+255E VERTICAL SINGLE AND RIGHT DOUBLE
    0x00f4u, // U+255F VERTICAL DOUBLE AND RIGHT SINGLE
    0x00fcu, // U+2560 DOUBLE VERTICAL AND RIGHT
    0x0053u, // U+2561 VERTICAL SINGLE AND LEFT DOUBLE
    0x00f1u, // U+2562 VERTICAL DOUBLE AND LEFT SINGLE
    0x00f3u, // U+2563 DOUBLE VERTICAL AND LEFT
    0x004fu, // U+2564 DOWN SINGLE AND HORIZONTAL DOUBLE
    0x00c5u, // U+2565 DOWN DOUBLE AND HORIZONTAL SINGLE
    0x00cfu, // U+2566 DOUBLE DOWN AND HORIZONTAL
    0x001fu, // U+2567 UP SINGLE AND HORIZONTAL DOUBLE
    0x0035u, // U+2568 UP DOUBLE AND HORIZONTAL SINGLE
    0x003fu, // U+2569 DOUBLE UP AND HORIZONTAL
    0x005fu, // U+256A VERTICAL SINGLE AND HORIZONTAL DOUBLE
    0x00f5u, // U+256B VERTICAL DOUBLE AND HORIZONTAL SINGLE
    0x00ffu, // U+256C DOUBLE VERTICAL AND HORIZONTAL
    0x1044u, // U+256D LIGHT ARC DOWN AND RIGHT
    0x1041u, // U+256E LIGHT ARC DOWN AND LEFT
    0x1011u, // U+256F LIGHT ARC UP AND LEFT
    0x1014u, // U+2570 LIGHT ARC UP AND RIGHT
    0x2000u, // U+2571 LIGHT DIAGONAL UPPER RIGHT TO LOWER LEFT
    0x4000u, // U+2572 LIGHT DIAGONAL UPPER LEFT TO LOWER RIGHT
    0x6000u, // U+2573 LIGHT DIAGONAL CROSS
    0x0001u, // U+2574 LIGHT LEFT
    0x0010u, // U+2575 LIGHT UP
    0x0004u, // U+2576 LIGHT RIGHT
    0x0040u, // U+2577 LIGHT DOWN
    0x0002u, // U+2578 HEAVY LEFT
    0x0020u, // U+2579 HEAVY UP
    0x0008u, // U+257A HEAVY RIGHT
    0x0080u, // U+257B HEAVY DOWN
    0x0009u, // U+257C LIGHT LEFT AND HEAVY RIGHT
    0x0090u, // U+257D LIGHT UP AND HEAVY DOWN
    0x0006u, // U+257E HEAVY LEFT AND LIGHT RIGHT
    0x0060u // U+257F HEAVY UP AND LIGHT DOWN
);

float rect_coverage(vec2 p, vec2 low, vec2 high) {
    // Integrate axis-aligned edges over a pixel; odd cell sizes must not
    // turn a half block or centered thin line into a missing pixel.
    vec2 covered = clamp(min(high, p + 0.5) - max(low, p - 0.5), 0.0, 1.0);
    return covered.x * covered.y;
}

float braille_coverage(uint pattern, vec2 p, vec2 size) {
    vec2 pitch = size / vec2(2.0, 4.0);
    uvec2 slot = uvec2(clamp(floor(p / pitch), vec2(0.0), vec2(1.0, 3.0)));
    const uint dots[8] = uint[](0u, 3u, 1u, 4u, 2u, 5u, 6u, 7u);
    uint dot = dots[slot.y * 2u + slot.x];
    if ((pattern & (1u << dot)) == 0u) return 0.0;
    vec2 center = (vec2(slot) + 0.5) * pitch;
    float radius = min(pitch.x, pitch.y) * 0.25;
    return clamp(radius + 0.5 - length(p - center), 0.0, 1.0);
}

float block_coverage(uint index, vec2 p, vec2 size) {
    if (index == 0u) return rect_coverage(p, vec2(0.0), vec2(size.x, size.y * 0.5));
    if (index <= 8u) return rect_coverage(p, vec2(0.0, size.y * (1.0 - float(index) / 8.0)), size);
    if (index <= 15u) return rect_coverage(p, vec2(0.0), vec2(size.x * float(16u - index) / 8.0, size.y));
    if (index == 16u) return rect_coverage(p, vec2(size.x * 0.5, 0.0), size);
    if (index <= 19u) {
        // A screen-aligned 2x2 pattern joins adjacent shaded cells, including
        // cells with odd strides. Density is 25%, 50%, or 75%.
        uvec2 pixel = uvec2(gl_FragCoord.xy);
        uint rank = ((pixel.x & 1u) << 1u) ^ ((pixel.y & 1u) * 3u);
        return rank < index - 16u ? 1.0 : 0.0;
    }
    if (index == 20u) return rect_coverage(p, vec2(0.0), vec2(size.x, size.y / 8.0));
    if (index == 21u) return rect_coverage(p, vec2(size.x * 7.0 / 8.0, 0.0), size);
    // Quadrant masks use TL, TR, BL, BR order; glyph order is not binary.
    const uint quadrants[10] = uint[](4u, 8u, 1u, 13u, 9u, 7u, 11u, 2u, 6u, 14u);
    uint mask = quadrants[index - 22u];
    float coverage = 0.0;
    for (uint quadrant = 0u; quadrant < 4u; ++quadrant) {
        if ((mask & (1u << quadrant)) == 0u) continue;
        vec2 low = vec2(quadrant & 1u, quadrant >> 1u) * size * 0.5;
        coverage += rect_coverage(p, low, low + size * 0.5);
    }
    return min(coverage, 1.0);
}

float arm_coverage(uint arm, uint weight, uint shape, vec2 p, vec2 size, float thin) {
    if (weight == 0u) return 0.0;
    bool horizontal = arm < 2u;
    bool positive = (arm & 1u) != 0u;
    // Snap light strokes to pixel boundaries. An even cell stride must not
    // blur a one-pixel line across two rows or close the double-line gap.
    vec2 center = floor(size * 0.5) + vec2(mod(thin, 2.0) * 0.5);
    float main_center = horizontal ? center.x : center.y;
    float cross_center = horizontal ? center.y : center.x;
    float main_extent = horizontal ? size.x : size.y;
    float result = 0.0;
    for (int stripe = 0; stripe < (weight == 3u ? 2 : 1); ++stripe) {
        float offset = weight == 3u ? (stripe == 0 ? -thin : thin) : 0.0;
        float thickness = weight == 2u ? 2.0 * thin : thin;
        float endpoint = main_center;
        uint negative_cross = (shape >> (horizontal ? 4u : 0u)) & 3u;
        uint positive_cross = (shape >> (horizontal ? 6u : 2u)) & 3u;
        if (weight == 3u) {
            // Double corners pair the inner and outer strokes, keeping the
            // gap open. Through junctions meet at the four offset corners.
            if (negative_cross == 3u && positive_cross == 3u) endpoint += positive ? thin : -thin;
            else if (positive_cross == 3u) endpoint += positive ? offset : -offset;
            else if (negative_cross == 3u) endpoint += positive ? -offset : offset;
        }
        if (weight != 3u && (negative_cross == 3u || positive_cross == 3u)) {
            endpoint += positive ? -thin : thin;
        }
        // Include the crossing stroke's half-width. Merely meeting at the
        // mathematical center leaves two half-covered pixels on odd strides.
        float low_main = positive ? endpoint - thin * 0.5 : 0.0;
        float high_main = positive ? main_extent : endpoint + thin * 0.5;
        float low_cross = cross_center + offset - thickness * 0.5;
        float high_cross = cross_center + offset + thickness * 0.5;
        vec2 low = horizontal ? vec2(low_main, low_cross) : vec2(low_cross, low_main);
        vec2 high = horizontal ? vec2(high_main, high_cross) : vec2(high_cross, high_main);
        result = max(result, rect_coverage(p, low, high));
    }
    return result;
}

float box_coverage(uint index, vec2 p, vec2 size) {
    uint shape = box_shapes[index];
    float thin = max(1.0, floor(min(size.x, size.y) / 8.0));
    uint diagonal = shape >> 13u;
    if (diagonal != 0u) {
        float norm = length(size);
        float a = abs(p.x * size.y + p.y * size.x - size.x * size.y) / norm;
        float b = abs(p.x * size.y - p.y * size.x) / norm;
        float distance = diagonal == 1u ? a : diagonal == 2u ? b : min(a, b);
        return clamp(thin * 0.5 + 0.5 - distance, 0.0, 1.0);
    }
    if ((shape & 0x1000u) != 0u) {
        bool right = ((shape >> 2u) & 3u) != 0u;
        bool down = ((shape >> 6u) & 3u) != 0u;
        vec2 center = floor(size * 0.5) + vec2(mod(thin, 2.0) * 0.5);
        float radius = min(size.x, size.y) * 0.25;
        vec2 sign = vec2(right ? 1.0 : -1.0, down ? 1.0 : -1.0);
        vec2 local = (p - center) * sign;
        if (local.x >= radius || local.y >= radius) {
            return max(arm_coverage(right ? 1u : 0u, 1u, shape, p, size, thin) * float(local.x >= radius),
                       arm_coverage(down ? 3u : 2u, 1u, shape, p, size, thin) * float(local.y >= radius));
        }
        float distance = abs(length(local - vec2(radius)) - radius);
        return clamp(thin * 0.5 + 0.5 - distance, 0.0, 1.0);
    }
    float coverage = 0.0;
    for (uint arm = 0u; arm < 4u; ++arm) {
        coverage = max(coverage, arm_coverage(arm, (shape >> (arm * 2u)) & 3u, shape, p, size, thin));
    }
    uint dashes = (shape >> 8u) & 7u;
    if (dashes != 0u) {
        bool horizontal = (shape & 15u) != 0u;
        float phase = fract((horizontal ? p.x / size.x : p.y / size.y) * float(dashes));
        coverage *= float(phase >= 0.15 && phase < 0.85);
    }
    return coverage;
}

// Unicode sextants number the 2x3 subcells in reading order: 12/34/56.
// The range skips masks 21 (left half) and 42 (right half), which already
// have U+258C/U+2590 encodings; space and full block are outside this range.
float sextant_coverage(uint scalar, vec2 p, vec2 size) {
    uint mask = scalar - 0x1fb00u + 1u;
    if (mask >= 21u) mask += 1u;
    if (mask >= 42u) mask += 1u;
    float coverage = 0.0;
    for (uint sixth = 0u; sixth < 6u; ++sixth) {
        if ((mask & (1u << sixth)) == 0u) continue;
        vec2 slot = vec2(sixth % 2u, sixth / 2u);
        vec2 low = slot * size / vec2(2.0, 3.0);
        vec2 high = (slot + 1.0) * size / vec2(2.0, 3.0);
        // Sum disjoint areas so adjacent thirds leave no antialiasing seam
        // when a pixel straddles their shared fractional boundary.
        coverage += rect_coverage(p, low, high);
    }
    return min(coverage, 1.0);
}

float graphic_coverage(uint scalar, vec2 p, vec2 size) {
    if (scalar >= 0x1fb00u) return sextant_coverage(scalar, p, size);
    if (scalar >= 0x2800u) return braille_coverage(scalar - 0x2800u, p, size);
    if (scalar >= 0x2580u) return block_coverage(scalar - 0x2580u, p, size);
    return box_coverage(scalar - 0x2500u, p, size);
}
