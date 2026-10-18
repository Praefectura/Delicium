#!/usr/bin/env python3
"""Deterministic generator for the `bangboo` example asset (0.2 skeleton3d).

The model is assembled from analytic primitives (closed ellipsoid shells and
one belt band), so the output has exact unit normals, watertight shells, no
degenerate triangles and no voxel blocks. Everything is local and
deterministic: no network, no downloads, no third-party packages (stdlib only),
and the same input always produces byte-identical output.

Design contract (kept in sync with the block that produced the asset):

* facing +Z, sole of the boots exactly at y = 0, ear tips at y = 1.65;
* nine joints: root -> body; body -> head, arm_l, arm_r, leg_l, leg_r;
  head -> ear_l, ear_r. Positive scales, unit quaternions, no cycles;
* vertex colours are linear RGB in [0, 1] with alpha exactly 1 (vertex-colour
  path: no images, no UVs, no material);
* five animations (`idle`, `walk`, `reaction`, `greet`, `play`) bound through
  the top-level `actions` table; `idle`/`walk` close their loops and the other
  three end on the rest pose;
* animation translation keys are authored as absolute *local* joint
  translations (bind translation as the base), never as model-space positions.

Usage:
    python -I tools/generate_bangboo.py                     # refuses to overwrite
    python -I tools/generate_bangboo.py --output <new path>
    python -I tools/generate_bangboo.py --force             # this asset only
    python -I tools/generate_bangboo.py --check <path>      # verify an existing file

Exit status is non-zero when any self-check fails.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Small vector / quaternion helpers (tuples, stdlib math only)
# ---------------------------------------------------------------------------


def v_add(a, b):
    return (a[0] + b[0], a[1] + b[1], a[2] + b[2])


def v_sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def v_dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def v_cross(a, b):
    return (
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    )


def v_len(a):
    return math.sqrt(v_dot(a, a))


def v_unit(a):
    n = v_len(a)
    if n == 0.0:
        raise ValueError("cannot normalize a zero vector")
    return (a[0] / n, a[1] / n, a[2] / n)


def q_mul(a, b):
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return (
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    )


def q_unit(q):
    n = math.sqrt(sum(c * c for c in q))
    if n < 1e-12:
        raise ValueError("zero quaternion")
    return tuple(c / n for c in q)


def q_axis(axis, degrees):
    """Unit quaternion of a rotation about a principal axis (degrees)."""
    half = math.radians(degrees) * 0.5
    s = math.sin(half)
    if axis == "x":
        return q_unit((s, 0.0, 0.0, math.cos(half)))
    if axis == "y":
        return q_unit((0.0, s, 0.0, math.cos(half)))
    if axis == "z":
        return q_unit((0.0, 0.0, s, math.cos(half)))
    raise ValueError(f"unknown axis {axis!r}")


def q_rot(q, v):
    """Rotate vector `v` by unit quaternion `q` (x, y, z, w)."""
    x, y, z, w = q
    tx = 2.0 * (y * v[2] - z * v[1])
    ty = 2.0 * (z * v[0] - x * v[2])
    tz = 2.0 * (x * v[1] - y * v[0])
    return (
        v[0] + w * tx + (y * tz - z * ty),
        v[1] + w * ty + (z * tx - x * tz),
        v[2] + w * tz + (x * ty - y * tx),
    )


Q_IDENTITY = (0.0, 0.0, 0.0, 1.0)

# ---------------------------------------------------------------------------
# Geometry constants (from the block spec; keep in sync with it)
# ---------------------------------------------------------------------------

BODY_CENTER = (0.0, 0.51, 0.0)
BODY_RADII = (0.34, 0.35, 0.30)
HEAD_CENTER = (0.0, 0.86, 0.0)
HEAD_RADII = (0.30, 0.30, 0.28)
SCREEN_CENTER = (0.0, 0.90, 0.24)
SCREEN_RADII = (0.21, 0.16, 0.07)
EYE_RADIUS = 0.058
EYE_X = 0.108
EYE_Y = 0.902
EYE_Z = 0.278   # embedded dome: ~0.036 proud of the visor surface

EAR_BASE_X = 0.095
EAR_BASE_Y = 1.05
EAR_TIP_X = 0.16
EAR_TIP_Y = 1.65
EAR_CROSS_X = 0.075    # lateral half-width of one ear
EAR_CROSS_Z = 0.055    # front/back half-thickness of one ear
INNER_EAR_RADII = (0.045, 0.20, 0.030)
INNER_EAR_Y_OFFSET = -0.02   # along the ear axis
INNER_EAR_Z_OFFSET = 0.030   # forward; the panel reads as a shallow relief

SHOULDER = (0.32, 0.62, 0.0)
HAND = (0.50, 0.44, 0.0)
ARM_CROSS = 0.075
HAND_RADIUS = 0.085

HIP = (0.15, 0.24, 0.0)
LEG_CENTER_Y = 0.16
LEG_RADII = (0.075, 0.115, 0.075)
BOOT_CENTER_Z = 0.018
BOOT_RADII = (0.085, 0.062, 0.105)   # bottom lands exactly on y = 0

BELT_Y_BOT = 0.283
BELT_Y_TOP = 0.338
BELT_OUT_MARGIN = 0.020
BELT_IN_MARGIN = -0.010
BELT_SEGMENTS = 24

BUCKLE_Y = 0.310
BUCKLE_RADII = (0.050, 0.042, 0.028)

# Linear vertex palette. Body/head/limbs off-white, matte black visor, bright
# teal eyes, warm orange belt hardware, charcoal boots, soft pink inner ears.
CREAM = (0.85, 0.80, 0.70)
BLACK = (0.03, 0.035, 0.04)
EYE = (0.20, 0.95, 0.85)
ORANGE = (0.62, 0.20, 0.04)
BOOT = (0.11, 0.11, 0.13)
PINK = (0.82, 0.66, 0.64)

# ---------------------------------------------------------------------------
# Skeleton (declared order is also topological order)
# ---------------------------------------------------------------------------

JOINTS = [
    # name, parent, local bind translation
    ("root", None, (0.0, 0.0, 0.0)),
    ("body", "root", BODY_CENTER),
    ("head", "body", (0.0, 0.35, 0.0)),            # world y = 0.86
    ("ear_l", "head", (EAR_BASE_X, 0.19, 0.0)),     # world (±0.095, 1.05)
    ("ear_r", "head", (-EAR_BASE_X, 0.19, 0.0)),
    ("arm_l", "body", (0.32, 0.11, 0.0)),          # world (±0.32, 0.62)
    ("arm_r", "body", (-0.32, 0.11, 0.0)),
    ("leg_l", "body", (HIP[0], HIP[1] - BODY_CENTER[1], 0.0)),   # world (±0.15, 0.24)
    ("leg_r", "body", (-HIP[0], HIP[1] - BODY_CENTER[1], 0.0)),
]

JOINT_INDEX = {name: i for i, (name, _, _) in enumerate(JOINTS)}
BIND_TRANSLATION = {name: t for name, _, t in JOINTS}

EXPECTED_PARENTS = {
    "root": None,
    "body": "root",
    "head": "body",
    "ear_l": "head",
    "ear_r": "head",
    "arm_l": "body",
    "arm_r": "body",
    "leg_l": "body",
    "leg_r": "body",
}

# ---------------------------------------------------------------------------
# Mesh builder
# ---------------------------------------------------------------------------


class MeshBuilder:
    """Accumulates vertices and triangles, keeping the module-level rules."""

    def __init__(self):
        self.vertices = []
        self.indices = []
        self.parts = []

    def _add_vertex(self, position, normal, color, joint):
        n = v_len(normal)
        if n < 1e-9:
            raise AssertionError("zero-length normal")
        normal = (normal[0] / n, normal[1] / n, normal[2] / n)
        self.vertices.append(
            {
                "position": [position[0], position[1], position[2]],
                "normal": [normal[0], normal[1], normal[2]],
                "color": [color[0], color[1], color[2], 1.0],
                "joints": [joint, joint, joint, joint],
                "weights": [1.0, 0.0, 0.0, 0.0],
            }
        )
        return len(self.vertices) - 1

    def _add_triangle(self, i0, i1, i2):
        if len({i0, i1, i2}) != 3:
            raise AssertionError("triangle repeats a vertex index")
        self.indices.extend((i0, i1, i2))

    def begin_part(self, name, joint):
        self._current = (name, joint, len(self.vertices), len(self.indices))

    def end_part(self, extra=None):
        name, joint, v0, i0 = self._current
        self.parts.append(
            {
                "part": name,
                "joint": JOINT_NAME(joint),
                "vertices": len(self.vertices) - v0,
                "triangles": (len(self.indices) - i0) // 3,
                "vertex_start": v0,
                "index_start": i0,
                "extra": extra or {},
            }
        )


def JOINT_NAME(index):
    return JOINTS[index][0]


def ellipsoid(
    mesh,
    name,
    joint,
    color,
    center,
    radii,
    rotation=Q_IDENTITY,
    stacks=10,
    segments=16,
):
    """Closed ellipsoid shell with analytic (exact) normals.

    Local frame: +Y pole first. Positions/normals are scaled, rotated and
    translated; the winding is counter-clockwise seen from outside, matching
    the verified convention of `robot3d.dmodel.json`.
    """
    rx, ry, rz = radii
    if min(rx, ry, rz) <= 0.0:
        raise AssertionError("ellipsoid radii must be positive")
    mesh.begin_part(name, joint)

    def emit(local_point):
        scaled = (local_point[0] * rx, local_point[1] * ry, local_point[2] * rz)
        grad = (
            scaled[0] / (rx * rx),
            scaled[1] / (ry * ry),
            scaled[2] / (rz * rz),
        )
        position = v_add(center, q_rot(rotation, scaled))
        normal = q_rot(rotation, grad)
        return mesh._add_vertex(position, normal, color, joint)

    north = emit((0.0, 1.0, 0.0))
    rings = []
    for i in range(1, stacks):
        theta = math.pi * i / stacks
        ring = []
        for j in range(segments):
            phi = 2.0 * math.pi * j / segments
            ring.append(
                emit(
                    (
                        math.sin(theta) * math.cos(phi),
                        math.cos(theta),
                        math.sin(theta) * math.sin(phi),
                    )
                )
            )
        rings.append(ring)
    south = emit((0.0, -1.0, 0.0))

    # North fan (phi order reversed, see the derivation in the module notes).
    first = rings[0]
    for j in range(segments):
        mesh._add_triangle(north, first[(j + 1) % segments], first[j])
    # Middle quads.
    for i in range(len(rings) - 1):
        a_ring, b_ring = rings[i], rings[i + 1]
        for j in range(segments):
            j1 = (j + 1) % segments
            mesh._add_triangle(a_ring[j], a_ring[j1], b_ring[j1])
            mesh._add_triangle(a_ring[j], b_ring[j1], b_ring[j])
    # South fan.
    last = rings[-1]
    for j in range(segments):
        mesh._add_triangle(south, last[j], last[(j + 1) % segments])

    mesh.end_part({"stacks": stacks, "segments": segments, "genus": 0})


def body_horizontal_radii(y):
    """Horizontal semi-axes of the body ellipsoid at height `y` (0 if outside)."""
    t = (y - BODY_CENTER[1]) / BODY_RADII[1]
    s = 1.0 - t * t
    if s <= 0.0:
        raise AssertionError("belt height outside the body ellipsoid")
    k = math.sqrt(s)
    return (BODY_RADII[0] * k, BODY_RADII[2] * k)


def belt_band(mesh, name, joint, color, y_bot, y_top, segments):
    """Closed sash: a rectangular-cross-section ring following the body ellipse.

    The inner face sits inside the body shell, so the band reads as a belt and
    keeps the mesh watertight (no open ends, no caps to weld)."""
    mesh.begin_part(name, joint)
    rx_bot, rz_bot = body_horizontal_radii(y_bot)
    rx_top, rz_top = body_horizontal_radii(y_top)

    rows = []
    normals = []
    for j in range(segments):
        phi = 2.0 * math.pi * j / segments
        c, s = math.cos(phi), math.sin(phi)
        out_bot = (rx_bot + BELT_OUT_MARGIN, rz_bot + BELT_OUT_MARGIN)
        out_top = (rx_top + BELT_OUT_MARGIN, rz_top + BELT_OUT_MARGIN)
        in_bot = (rx_bot + BELT_IN_MARGIN, rz_bot + BELT_IN_MARGIN)
        in_top = (rx_top + BELT_IN_MARGIN, rz_top + BELT_IN_MARGIN)

        def radial(radii, y):
            return (radii[0] * c, y, radii[1] * s)

        # Outward radial direction of the ellipse (x/rx^2, 0, z/rz^2).
        n_out_bot = v_unit((c / out_bot[0], 0.0, s / out_bot[1]))
        n_out_top = v_unit((c / out_top[0], 0.0, s / out_top[1]))
        n_in_bot = (-n_out_bot[0], 0.0, -n_out_bot[2])
        n_in_top = (-n_out_top[0], 0.0, -n_out_top[2])

        up = (0.0, 1.0, 0.0)
        down = (0.0, -1.0, 0.0)

        corners = [
            (radial(out_top, y_top), v_unit(v_add(n_out_top, up))),   # k = 0
            (radial(out_bot, y_bot), v_unit(v_add(n_out_bot, down))),  # k = 1
            (radial(in_bot, y_bot), v_unit(v_add(n_in_bot, down))),   # k = 2
            (radial(in_top, y_top), v_unit(v_add(n_in_top, up))),     # k = 3
        ]
        row = []
        nrow = []
        for position, normal in corners:
            row.append(mesh._add_vertex(position, normal, color, joint))
            nrow.append(normal)
        rows.append(row)
        normals.append(nrow)

    for j in range(segments):
        j1 = (j + 1) % segments
        for k in range(4):
            k1 = (k + 1) % 4
            # Verified winding for this parametrization (see module notes).
            mesh._add_triangle(rows[j][k], rows[j1][k], rows[j1][k1])
            mesh._add_triangle(rows[j][k], rows[j1][k1], rows[j][k1])

    mesh.end_part({"segments": segments, "cross_section": "rectangle", "genus": 1})


def build_mesh():
    mesh = MeshBuilder()

    # Body and head: the two main masses.
    ellipsoid(mesh, "body", JOINT_INDEX["body"], CREAM, BODY_CENTER, BODY_RADII,
              stacks=12, segments=24)
    ellipsoid(mesh, "head", JOINT_INDEX["head"], CREAM, HEAD_CENTER, HEAD_RADII,
              stacks=12, segments=24)

    # Curved black visor, embedded in the head and bulging forward.
    ellipsoid(mesh, "screen", JOINT_INDEX["head"], BLACK, SCREEN_CENTER,
              SCREEN_RADII, stacks=9, segments=20)

    # Bright teal eyes, proud of the visor surface (no coplanar overlap).
    for side, sign in (("l", 1.0), ("r", -1.0)):
        ellipsoid(mesh, f"eye_{side}", JOINT_INDEX["head"], EYE,
                  (sign * EYE_X, EYE_Y, EYE_Z),
                  (EYE_RADIUS, EYE_RADIUS, EYE_RADIUS),
                  stacks=7, segments=14)

    # Long rabbit ears, tilted outward, plus a soft inner-ear panel on the
    # front face of each ear.
    ex = EAR_TIP_X - EAR_BASE_X
    ey = EAR_TIP_Y - EAR_BASE_Y
    ear_length = math.hypot(ex, ey)
    tilt_deg = math.degrees(math.atan2(ex, ey))
    for side, sign in (("l", 1.0), ("r", -1.0)):
        rot = q_axis("z", -tilt_deg * sign)
        center = (sign * (EAR_BASE_X + ex * 0.5), EAR_BASE_Y + ey * 0.5, 0.0)
        ellipsoid(mesh, f"ear_{side}", JOINT_INDEX[f"ear_{side}"], CREAM,
                  center, (EAR_CROSS_X, ear_length * 0.5, EAR_CROSS_Z),
                  rotation=rot, stacks=10, segments=14)
        inner_center = v_add(
            center,
            q_rot(rot, (0.0, INNER_EAR_Y_OFFSET, INNER_EAR_Z_OFFSET)),
        )
        ellipsoid(mesh, f"ear_inner_{side}", JOINT_INDEX[f"ear_{side}"], PINK,
                  inner_center, INNER_EAR_RADII,
                  rotation=rot, stacks=8, segments=12)

    # Short arms with mitten hands.
    arm_dir = v_sub(HAND, SHOULDER)
    arm_length = v_len(arm_dir)
    for side, sign in (("l", 1.0), ("r", -1.0)):
        # Local +Y maps onto the authored down/outward arm direction.
        rot = q_axis("z", -135.0 * sign)
        center = tuple((SHOULDER[i] + HAND[i]) * 0.5 for i in range(3))
        center = (center[0] * sign, center[1], center[2])
        ellipsoid(mesh, f"arm_{side}", JOINT_INDEX[f"arm_{side}"], CREAM,
                  center, (ARM_CROSS, arm_length * 0.5, ARM_CROSS),
                  rotation=rot, stacks=8, segments=12)
        ellipsoid(mesh, f"hand_{side}", JOINT_INDEX[f"arm_{side}"], CREAM,
                  (sign * HAND[0], HAND[1], HAND[2]),
                  (HAND_RADIUS, HAND_RADIUS, HAND_RADIUS),
                  stacks=7, segments=12)

    # Short legs and rounded boots; the sole lands exactly on y = 0.
    for side, sign in (("l", 1.0), ("r", -1.0)):
        ellipsoid(mesh, f"leg_{side}", JOINT_INDEX[f"leg_{side}"], CREAM,
                  (sign * HIP[0], LEG_CENTER_Y, 0.0), LEG_RADII,
                  stacks=8, segments=12)
        ellipsoid(mesh, f"boot_{side}", JOINT_INDEX[f"leg_{side}"], BOOT,
                  (sign * HIP[0], BOOT_RADII[1], BOOT_CENTER_Z), BOOT_RADII,
                  stacks=9, segments=16)

    # Orange belt hardware: a sash around the waist plus a round buckle.
    belt_band(mesh, "belt", JOINT_INDEX["body"], ORANGE, BELT_Y_BOT, BELT_Y_TOP,
              BELT_SEGMENTS)
    rx, rz = body_horizontal_radii(BUCKLE_Y)
    ellipsoid(mesh, "buckle", JOINT_INDEX["body"], ORANGE,
              (0.0, BUCKLE_Y, rz + BELT_OUT_MARGIN + BUCKLE_RADII[2] * 0.5),
              BUCKLE_RADII, stacks=7, segments=12)

    return mesh

# ---------------------------------------------------------------------------
# Animations
# ---------------------------------------------------------------------------


def channel(joint, translation=None, rotation=None, scale=None):
    def keys(values):
        return [{"time": t, "value": list(v)} for t, v in values]

    return {
        "joint": joint,
        "interpolation": "linear",
        "translation": keys(translation or []),
        "rotation": keys(rotation or []),
        "scale": keys(scale or []),
    }


def t_keys(joint, pairs):
    """Local translation keys: values are absolute local joint translations."""
    return channel(joint, translation=pairs)


def r_keys(joint, pairs):
    return channel(joint, rotation=[(t, q_unit(q)) for t, q in pairs])


def s_keys(joint, pairs):
    return channel(joint, scale=pairs)


def joint_channel(joint, translation=None, rotation=None, scale=None):
    """One channel per joint (the validator rejects duplicates): the tracks of
    a joint are always authored together, never as sibling channels."""
    return channel(joint, translation=translation, rotation=rotation, scale=scale)


def body_breathe(k):
    """Breathing pose of the body: widen slightly and rise without lifting the
    boots. Scaling about the body joint moves every child, so the y translation
    is compensated (T_y = 0.51 * s_y) to keep the soles on the floor."""
    s = (1.0 + 0.012 * k, 1.0 + 0.008 * k, 1.0 + 0.012 * k)
    t = (0.0, BODY_CENTER[1] * s[1], 0.0)
    return t, s


def squash(k):
    """Body squash/stretch of amount `k` (>0 squash, <0 stretch), feet planted."""
    s = (1.0 + 0.07 * k, 1.0 - 0.10 * k, 1.0 + 0.07 * k)
    t = (0.0, BODY_CENTER[1] * s[1], 0.0)
    return t, s


def build_animations():
    idle_breathe_0 = body_breathe(0.0)
    idle_breathe_1 = body_breathe(1.0)

    idle = {
        "name": "idle",
        "duration": 2.5,
        "channels": [
            joint_channel(
                "body",
                translation=[(0.0, idle_breathe_0[0]), (1.25, idle_breathe_1[0]),
                             (2.5, idle_breathe_0[0])],
                scale=[(0.0, idle_breathe_0[1]), (1.25, idle_breathe_1[1]),
                       (2.5, idle_breathe_0[1])],
            ),
            # Ears counter-sway against each other.
            r_keys("ear_l", [(0.0, Q_IDENTITY), (0.625, q_axis("z", 2.5)),
                             (1.25, Q_IDENTITY), (1.875, q_axis("z", -2.5)),
                             (2.5, Q_IDENTITY)]),
            r_keys("ear_r", [(0.0, Q_IDENTITY), (0.625, q_axis("z", -2.5)),
                             (1.25, Q_IDENTITY), (1.875, q_axis("z", 2.5)),
                             (2.5, Q_IDENTITY)]),
        ],
    }

    walk_bob_low = BIND_TRANSLATION["body"]
    walk_bob_high = (0.0, BIND_TRANSLATION["body"][1] + 0.02, 0.0)
    walk = {
        "name": "walk",
        "duration": 1.0,
        "channels": [
            t_keys("body", [(0.0, walk_bob_low), (0.25, walk_bob_high),
                            (0.5, walk_bob_low), (0.75, walk_bob_high),
                            (1.0, walk_bob_low)]),
            r_keys("leg_l", [(0.0, Q_IDENTITY), (0.25, q_axis("x", 20.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("x", -20.0)),
                             (1.0, Q_IDENTITY)]),
            r_keys("leg_r", [(0.0, Q_IDENTITY), (0.25, q_axis("x", -20.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("x", 20.0)),
                             (1.0, Q_IDENTITY)]),
            r_keys("arm_l", [(0.0, Q_IDENTITY), (0.25, q_axis("x", -15.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("x", 15.0)),
                             (1.0, Q_IDENTITY)]),
            r_keys("arm_r", [(0.0, Q_IDENTITY), (0.25, q_axis("x", 15.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("x", -15.0)),
                             (1.0, Q_IDENTITY)]),
            r_keys("ear_l", [(0.0, Q_IDENTITY), (0.25, q_axis("z", 4.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("z", -4.0)),
                             (1.0, Q_IDENTITY)]),
            r_keys("ear_r", [(0.0, Q_IDENTITY), (0.25, q_axis("z", -4.0)),
                             (0.5, Q_IDENTITY), (0.75, q_axis("z", 4.0)),
                             (1.0, Q_IDENTITY)]),
        ],
    }

    squash_t, squash_s = squash(1.0)
    stretch_t, stretch_s = squash(-0.5)
    rest_t = BIND_TRANSLATION["body"]
    reaction = {
        "name": "reaction",
        "duration": 1.0,
        "channels": [
            joint_channel(
                "body",
                translation=[(0.0, rest_t), (0.12, squash_t), (0.38, stretch_t),
                             (0.62, rest_t), (1.0, rest_t)],
                scale=[(0.0, (1.0, 1.0, 1.0)), (0.12, squash_s),
                       (0.38, stretch_s), (0.62, (1.0, 1.0, 1.0)),
                       (1.0, (1.0, 1.0, 1.0))],
            ),
            # Both ears snap upright (they rest 6.18 degrees outward) and
            # stretch a little.
            joint_channel(
                "ear_l",
                rotation=[(0.0, Q_IDENTITY), (0.12, q_axis("z", 7.0)),
                          (0.70, q_axis("z", 7.0)), (1.0, Q_IDENTITY)],
                scale=[(0.0, (1.0, 1.0, 1.0)), (0.12, (1.0, 1.10, 1.0)),
                       (0.70, (1.0, 1.10, 1.0)), (1.0, (1.0, 1.0, 1.0))],
            ),
            joint_channel(
                "ear_r",
                rotation=[(0.0, Q_IDENTITY), (0.12, q_axis("z", -7.0)),
                          (0.70, q_axis("z", -7.0)), (1.0, Q_IDENTITY)],
                scale=[(0.0, (1.0, 1.0, 1.0)), (0.12, (1.0, 1.10, 1.0)),
                       (0.70, (1.0, 1.10, 1.0)), (1.0, (1.0, 1.0, 1.0))],
            ),
            # Arms come up to shoulder height.
            r_keys("arm_l", [(0.0, Q_IDENTITY), (0.14, q_axis("z", 42.0)),
                             (0.72, q_axis("z", 42.0)), (1.0, Q_IDENTITY)]),
            r_keys("arm_r", [(0.0, Q_IDENTITY), (0.14, q_axis("z", -42.0)),
                             (0.72, q_axis("z", -42.0)), (1.0, Q_IDENTITY)]),
        ],
    }

    greet = {
        "name": "greet",
        "duration": 1.6,
        "channels": [
            # One small hop (presentation-only root offset, 0.05 <= 0.06).
            t_keys("root", [(0.0, (0.0, 0.0, 0.0)), (0.22, (0.0, 0.0, 0.0)),
                            (0.34, (0.0, 0.05, 0.0)), (0.48, (0.0, 0.0, 0.0))]),
            # Right arm raised, waving up and down twice.
            r_keys("arm_r", [(0.0, Q_IDENTITY), (0.25, q_axis("z", -40.0)),
                             (0.45, q_axis("z", -52.0)), (0.65, q_axis("z", -28.0)),
                             (0.85, q_axis("z", -52.0)), (1.05, q_axis("z", -40.0)),
                             (1.30, Q_IDENTITY)]),
            # Ears nod in step with the wave.
            r_keys("ear_l", [(0.0, Q_IDENTITY), (0.25, q_axis("x", 9.0)),
                             (0.45, q_axis("x", -9.0)), (0.65, q_axis("x", 9.0)),
                             (0.85, q_axis("x", -9.0)), (1.05, q_axis("x", 5.0)),
                             (1.30, Q_IDENTITY)]),
            r_keys("ear_r", [(0.0, Q_IDENTITY), (0.25, q_axis("x", 9.0)),
                             (0.45, q_axis("x", -9.0)), (0.65, q_axis("x", 9.0)),
                             (0.85, q_axis("x", -9.0)), (1.05, q_axis("x", 5.0)),
                             (1.30, Q_IDENTITY)]),
        ],
    }

    play_squash_t, play_squash_s = squash(0.6)
    play_air_t, play_air_s = squash(-0.3)
    play = {
        "name": "play",
        "duration": 2.2,
        "channels": [
            # Two hops, then a sway; the root offset never exceeds 0.06.
            t_keys("root", [(0.0, (0.0, 0.0, 0.0)), (0.26, (0.0, 0.0, 0.0)),
                            (0.40, (0.0, 0.06, 0.0)), (0.54, (0.0, 0.0, 0.0)),
                            (0.78, (0.0, 0.0, 0.0)), (0.92, (0.0, 0.06, 0.0)),
                            (1.06, (0.0, 0.0, 0.0))]),
            # Body: hop squash/stretch, then a sway; the y translation keeps the
            # boots planted for every scale key. The sway is a yaw twist, not a
            # roll: a rigid roll about the body axis would dip the outer foot
            # below the floor, while a twist keeps every sole height untouched.
            joint_channel(
                "body",
                translation=[(0.0, rest_t), (0.50, play_air_t), (0.60, play_squash_t),
                             (0.72, rest_t), (1.02, play_air_t), (1.12, play_squash_t),
                             (1.24, rest_t), (2.20, rest_t)],
                rotation=[(0.0, Q_IDENTITY), (1.20, Q_IDENTITY),
                          (1.50, q_axis("y", 9.0)), (1.80, q_axis("y", -9.0)),
                          (2.05, q_axis("y", 2.5)), (2.20, Q_IDENTITY)],
                scale=[(0.0, (1.0, 1.0, 1.0)), (0.50, play_air_s),
                       (0.60, play_squash_s), (0.72, (1.0, 1.0, 1.0)),
                       (1.02, play_air_s), (1.12, play_squash_s),
                       (1.24, (1.0, 1.0, 1.0)),
                       (1.55, (1.025, 0.99, 1.025)), (1.85, (0.99, 1.01, 0.99)),
                       (2.20, (1.0, 1.0, 1.0))],
            ),
            # Head counter-twists so it mostly holds its heading while the
            # body wiggles.
            r_keys("head", [(0.0, Q_IDENTITY), (1.20, Q_IDENTITY),
                            (1.50, q_axis("y", -6.0)), (1.80, q_axis("y", 6.0)),
                            (2.05, q_axis("y", -2.0)), (2.20, Q_IDENTITY)]),
            # Ears flop in counter-phase.
            r_keys("ear_l", [(0.0, Q_IDENTITY), (0.40, q_axis("z", 11.0)),
                             (0.70, q_axis("z", -11.0)), (1.00, q_axis("z", 11.0)),
                             (1.30, q_axis("z", -11.0)), (1.60, q_axis("z", 9.0)),
                             (1.90, q_axis("z", -7.0)), (2.20, Q_IDENTITY)]),
            r_keys("ear_r", [(0.0, Q_IDENTITY), (0.40, q_axis("z", -11.0)),
                             (0.70, q_axis("z", 11.0)), (1.00, q_axis("z", -11.0)),
                             (1.30, q_axis("z", 11.0)), (1.60, q_axis("z", -9.0)),
                             (1.90, q_axis("z", 7.0)), (2.20, Q_IDENTITY)]),
            r_keys("arm_l", [(0.0, Q_IDENTITY), (0.40, q_axis("x", -16.0)),
                             (0.70, q_axis("x", 16.0)), (1.00, q_axis("x", -16.0)),
                             (1.30, q_axis("x", 10.0)), (1.60, q_axis("x", -10.0)),
                             (2.20, Q_IDENTITY)]),
            r_keys("arm_r", [(0.0, Q_IDENTITY), (0.40, q_axis("x", 16.0)),
                             (0.70, q_axis("x", -16.0)), (1.00, q_axis("x", 16.0)),
                             (1.30, q_axis("x", -10.0)), (1.60, q_axis("x", 10.0)),
                             (2.20, Q_IDENTITY)]),
        ],
    }

    return [idle, walk, reaction, greet, play]


# ---------------------------------------------------------------------------
# Document assembly
# ---------------------------------------------------------------------------


def build_document():
    mesh = build_mesh()
    animations = build_animations()
    doc = {
        "format": "delicium.model",
        "format_version": "0.2",
        "name": "bangboo",
        "kind": "skeleton3d",
        "interaction": {"head_joint": "head"},
        "actions": {
            "idle": "idle",
            "walk": "walk",
            "reaction": "reaction",
            "greet": "greet",
            "play": "play",
        },
        "skeleton3d": {
            "vertices": mesh.vertices,
            "indices": mesh.indices,
            "joints": [
                {
                    "name": name,
                    "parent": parent,
                    "translation": list(translation),
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0],
                }
                for name, parent, translation in JOINTS
            ],
            "animations": animations,
        },
    }
    return doc, mesh


# ---------------------------------------------------------------------------
# Self checks (mirror of the loader rules in src/model/{schema_v02,validate}.rs)
# ---------------------------------------------------------------------------

MAX_ABS_3D = 1e6
MIN_ABS_SCALE = 1e-6
MAX_NAME_LEN = 128
TRI_AREA_EPS = 1e-6

ALLOWED_KEYS = {
    "envelope": {"format", "format_version", "name", "kind", "interaction",
                 "actions", "images", "sprite2d", "skeleton3d"},
    "actions": {"idle", "walk", "reaction", "greet", "play"},
    "interaction": {"gaze_parameter", "head_joint"},
    "skeleton3d": {"vertices", "indices", "joints", "animations", "texture",
                   "uvs", "material"},
    "vertex": {"position", "normal", "color", "joints", "weights"},
    "joint": {"name", "parent", "translation", "rotation", "scale"},
    "animation": {"name", "duration", "channels"},
    "channel": {"joint", "interpolation", "translation", "rotation", "scale"},
    "key": {"time", "value"},
}

ONESHOT_CLIPS = {"reaction", "greet", "play"}
LOOP_CLIPS = {"idle", "walk"}


class CheckAbort(Exception):
    """Raised on a structural failure that makes later checks meaningless."""


class Checker:
    def __init__(self):
        self.checks = []
        self.failures = []

    def check(self, name, condition, detail=""):
        ok = bool(condition)
        self.checks.append({"check": name, "pass": ok})
        if not ok:
            self.failures.append(f"{name}: {detail}")
        return ok

    def require(self, name, condition, detail=""):
        if not self.check(name, condition, detail):
            raise CheckAbort(name)


def is_finite(x):
    return isinstance(x, (int, float)) and math.isfinite(x)


def close(a, b, eps=1e-6):
    return abs(a - b) <= eps


def vec_close(a, b, eps=1e-6):
    return len(a) == len(b) and all(close(x, y, eps) for x, y in zip(a, b))


def triangle_area_ok(a, b, c):
    e1 = tuple(float(b[i] - a[i]) for i in range(3))
    e2 = tuple(float(c[i] - a[i]) for i in range(3))
    cross = v_cross(e1, e2)
    area = v_len(cross)
    scale = v_len(e1) * v_len(e2)
    return scale > 0.0 and area > TRI_AREA_EPS * scale


def self_check(doc, mesh=None, *, checker=None):
    c = checker if checker is not None else Checker()
    c.require("envelope/keys", set(doc) <= ALLOWED_KEYS["envelope"],
              f"unknown keys {sorted(set(doc) - ALLOWED_KEYS['envelope'])}")
    c.require("envelope/format", doc.get("format") == "delicium.model", "bad format")
    c.require("envelope/version", doc.get("format_version") == "0.2", "bad version")
    name = doc.get("name", "")
    c.require("envelope/name",
              0 < len(name) <= MAX_NAME_LEN and all(ord(ch) >= 32 for ch in name),
              f"bad name {name!r}")
    c.require("envelope/kind", doc.get("kind") == "skeleton3d", "bad kind")
    c.check("envelope/no-images", "images" not in doc or not doc["images"],
            "images present")
    c.require("envelope/skeleton3d", isinstance(doc.get("skeleton3d"), dict),
              "missing skeleton3d payload")

    interaction = doc.get("interaction", {})
    c.require("interaction/keys", set(interaction) <= ALLOWED_KEYS["interaction"],
              "unknown interaction keys")
    c.require("interaction/head-joint", interaction.get("head_joint") == "head",
              "head_joint must be `head`")

    actions = doc.get("actions")
    c.require("actions/present", isinstance(actions, dict), "actions table missing")
    c.require("actions/keys", set(actions) <= ALLOWED_KEYS["actions"],
              "unknown action roles")
    for role in ("idle", "walk", "reaction", "greet", "play"):
        c.require(f"actions/{role}", role in actions, "role not bound")

    skel = doc["skeleton3d"]
    c.require("skeleton3d/keys", set(skel) <= ALLOWED_KEYS["skeleton3d"],
              f"unknown keys {sorted(set(skel) - ALLOWED_KEYS['skeleton3d'])}")
    c.check("skeleton3d/no-uvs", "uvs" not in skel, "uvs present")
    c.check("skeleton3d/no-material", "material" not in skel, "material present")
    c.check("skeleton3d/no-texture", "texture" not in skel, "texture present")

    # --- joints ---------------------------------------------------------
    joints = skel["joints"]
    names = [j["name"] for j in joints]
    c.require("joints/count", len(joints) == 9, f"{len(joints)} joints, expected 9")
    c.require("joints/unique", len(set(names)) == len(names), "duplicate joint name")
    for i, joint in enumerate(joints):
        ctx = f"joints[{i}]"
        c.require(f"{ctx}/keys", set(joint) <= ALLOWED_KEYS["joint"], "unknown keys")
        c.check(f"{ctx}/translation-finite",
                all(is_finite(v) and abs(v) <= MAX_ABS_3D for v in joint["translation"]),
                "bad translation")
        c.check(f"{ctx}/rotation-finite",
                all(is_finite(v) for v in joint["rotation"]), "bad rotation")
        n2 = sum(v * v for v in joint["rotation"])
        c.check(f"{ctx}/rotation-magnitude", n2 >= 1e-6, f"|q|^2 = {n2}")
        c.check(f"{ctx}/rotation-unit", abs(math.sqrt(n2) - 1.0) <= 1e-6,
                f"|q| = {math.sqrt(n2)}")
        c.check(f"{ctx}/scale-positive",
                all(is_finite(v) and v > MIN_ABS_SCALE and v <= MAX_ABS_3D
                    for v in joint["scale"]), "bad scale")
    by_name = {j["name"]: j for j in joints}
    c.check("joints/expected-names", set(by_name) == set(EXPECTED_PARENTS),
            f"unexpected joints {sorted(set(by_name) ^ set(EXPECTED_PARENTS))}")
    for joint in joints:
        c.check(f"joint/{joint['name']}/hierarchy",
                joint["parent"] == EXPECTED_PARENTS[joint["name"]],
                f"parent is {joint['parent']!r}")
    # acyclicity: walk up from every joint.
    for joint in joints:
        seen = set()
        cur = joint["name"]
        while cur is not None:
            c.require(f"joint/{joint['name']}/acyclic", cur in by_name,
                      f"parent `{cur}` is not a declared joint")
            c.require(f"joint/{joint['name']}/acyclic-step", cur not in seen,
                      "cycle in the joint graph")
            seen.add(cur)
            cur = by_name[cur]["parent"]
    c.check("joints/one-root",
            sum(1 for j in joints if j["parent"] is None) == 1, "expected one root")

    # --- vertices -------------------------------------------------------
    verts = skel["vertices"]
    c.require("vertices/count", len(verts) > 0, "no vertices")
    c.check("vertices/budget", len(verts) < 5000, f"{len(verts)} vertices (budget 5000)")
    used_joints = set()
    bad_keys = []
    bad_position = []
    bad_normal = []
    bad_color = []
    bad_alpha = []
    bad_indices = []
    bad_weights = []
    for i, v in enumerate(verts):
        if set(v) > ALLOWED_KEYS["vertex"]:
            bad_keys.append(f"v{i}: {sorted(set(v) - ALLOWED_KEYS['vertex'])}")
        if not all(is_finite(x) and abs(x) <= MAX_ABS_3D for x in v["position"]):
            bad_position.append(f"v{i}: non-finite position")
        n2 = sum(x * x for x in v["normal"])
        if n2 < 1e-8:
            bad_normal.append(f"v{i}: zero normal")
        elif abs(math.sqrt(n2) - 1.0) > 2e-6:
            bad_normal.append(f"v{i}: |n|={math.sqrt(n2):.8f}")
        color = v["color"]
        if not all(is_finite(x) and 0.0 <= x <= 1.0 for x in color):
            bad_color.append(f"v{i}: color {color}")
        if color[3] != 1.0:
            bad_alpha.append(f"v{i}: alpha {color[3]}")
        if not all(0 <= j < len(joints) for j in v["joints"]):
            bad_indices.append(f"v{i}: joints {v['joints']}")
        weights = v["weights"]
        if any((not is_finite(w)) or w < 0.0 for w in weights) or sum(weights) <= 0.0:
            bad_weights.append(f"v{i}: weights {weights}")
        else:
            used_joints.update(j for j, w in zip(v["joints"], weights) if w > 0.0)
    c.check("vertices/keys", not bad_keys, f"first: {bad_keys[:1]}")
    c.check("vertices/positions", not bad_position, f"first: {bad_position[:1]}")
    c.check("vertices/normals", not bad_normal, f"first: {bad_normal[:1]}")
    c.check("vertices/colors", not bad_color, f"first: {bad_color[:1]}")
    c.check("vertices/alpha-opaque", not bad_alpha, f"first: {bad_alpha[:1]}")
    c.check("vertices/joint-indices", not bad_indices, f"first: {bad_indices[:1]}")
    c.check("vertices/weights", not bad_weights, f"first: {bad_weights[:1]}")
    c.check("vertices/all-parts-weighted", used_joints == set(range(len(joints))) - {0},
            f"joints without weighted vertices: "
            f"{sorted((set(range(len(joints))) - {0}) - used_joints)} "
            f"(root is a transform-only joint and carries no geometry)")

    # --- triangles ------------------------------------------------------
    idx = skel["indices"]
    c.require("indices/multiple-of-3", len(idx) % 3 == 0, "indices not triangles")
    c.check("indices/budget", len(idx) // 3 < 8000,
            f"{len(idx) // 3} triangles (budget 8000)")
    c.require("indices/range", all(0 <= i < len(verts) for i in idx),
              "index out of range")
    min_area = math.inf
    bad_area = []
    bad_winding = []
    for t in range(len(idx) // 3):
        i0, i1, i2 = idx[3 * t:3 * t + 3]
        if len({i0, i1, i2}) != 3:
            bad_area.append(f"t{t}: repeated index")
            continue
        a = verts[i0]["position"]
        b = verts[i1]["position"]
        if not triangle_area_ok(a, b, verts[i2]["position"]):
            bad_area.append(f"t{t}: degenerate")
            continue
        face = v_cross(v_sub(b, a), v_sub(verts[i2]["position"], a))
        length = v_len(face)
        min_area = min(min_area, length * 0.5)
        avg = (0.0, 0.0, 0.0)
        for i in (i0, i1, i2):
            avg = v_add(avg, verts[i]["normal"])
        if v_dot(face, avg) <= 0.0:
            bad_winding.append(f"t{t}")
    c.check("triangles/non-degenerate", not bad_area, f"first: {bad_area[:3]}")
    c.check("triangles/min-area", min_area > 1e-6, f"min area {min_area}")
    c.check("triangles/winding", not bad_winding,
            f"{len(bad_winding)} triangles wound against their vertex normals: "
            f"{bad_winding[:3]}")
    referenced = set(idx)
    c.check("triangles/all-vertices-referenced",
            len(referenced) == len(verts),
            f"{len(verts) - len(referenced)} unreferenced vertices")

    # --- animations -----------------------------------------------------
    anims = skel["animations"]
    c.require("animations/count", len(anims) == 5, f"{len(anims)} animations, expected 5")
    anim_names = [a["name"] for a in anims]
    c.require("animations/unique", len(set(anim_names)) == len(anim_names),
              "duplicate animation name")
    for role in ("idle", "walk", "reaction", "greet", "play"):
        c.require(f"animations/{role}/bound", actions[role] in anim_names,
                  f"actions.{role} -> {actions[role]!r} not found")
    total_keys = 0
    for anim in anims:
        ctx = f"animations.{anim['name']}"
        c.require(f"{ctx}/keys", set(anim) <= ALLOWED_KEYS["animation"], "unknown keys")
        duration = anim["duration"]
        c.require(f"{ctx}/duration", is_finite(duration) and duration > 0.0,
                  "bad duration")
        c.require(f"{ctx}/channels", len(anim["channels"]) > 0, "no channels")
        channel_joints = [ch["joint"] for ch in anim["channels"]]
        c.check(f"{ctx}/unique-channels", len(set(channel_joints)) == len(channel_joints),
                "duplicate channel joint")
        for ch in anim["channels"]:
            cctx = f"{ctx}.channels[{ch['joint']}]"
            c.require(f"{cctx}/keys", set(ch) <= ALLOWED_KEYS["channel"], "unknown keys")
            c.require(f"{cctx}/joint", ch["joint"] in by_name, "unknown joint")
            c.require(f"{cctx}/interpolation", ch["interpolation"] == "linear",
                      "expected linear interpolation")
            tracks = (("translation", ch["translation"]), ("rotation", ch["rotation"]),
                      ("scale", ch["scale"]))
            c.require(f"{cctx}/nonempty",
                      any(len(keys) > 0 for _, keys in tracks),
                      "channel has no keys")
            for kind, keys in tracks:
                if not keys:
                    continue
                kctx = f"{cctx}.{kind}"
                previous = -math.inf
                for k, key in enumerate(keys):
                    c.require(f"{kctx}[{k}]/keys", set(key) <= ALLOWED_KEYS["key"],
                              "unknown keys")
                    time = key["time"]
                    c.check(f"{kctx}[{k}]/time",
                            is_finite(time) and 0.0 <= time <= duration and time > previous,
                            f"time {time} after {previous} (duration {duration})")
                    c.check(f"{kctx}[{k}]/value-finite",
                            all(is_finite(x) and abs(x) <= MAX_ABS_3D for x in key["value"]),
                            "non-finite value")
                    previous = time
                c.check(f"{kctx}/first-key-at-zero", close(keys[0]["time"], 0.0),
                        f"first key at t = {keys[0]['time']}")
                total_keys += len(keys)
            # Quaternion keys must be unit.
            for k, key in enumerate(ch["rotation"]):
                n2 = sum(x * x for x in key["value"])
                c.check(f"{cctx}/quat[{k}]", abs(math.sqrt(n2) - 1.0) <= 1e-6,
                        f"|q| = {math.sqrt(n2)}")
            # Scale keys must keep the bind sign and stay away from zero.
            bind_scale = by_name[ch["joint"]]["scale"]
            for k, key in enumerate(ch["scale"]):
                for comp, bind in enumerate(bind_scale):
                    value = key["value"][comp]
                    c.check(f"{cctx}/scale[{k}][{comp}]",
                            abs(value) >= MIN_ABS_SCALE and
                            (value < 0.0) == (bind < 0.0),
                            f"scale sign/zero violation at component {comp}")
        # Loop/one-shot closure.
        if anim["name"] in LOOP_CLIPS:
            for ch in anim["channels"]:
                for kind in ("translation", "rotation", "scale"):
                    keys = ch[kind]
                    if not keys:
                        continue
                    c.check(f"{ctx}/{ch['joint']}/{kind}-loop",
                            vec_close(keys[0]["value"], keys[-1]["value"], 1e-6),
                            "first and last key differ")
        if anim["name"] in ONESHOT_CLIPS:
            for ch in anim["channels"]:
                for kind in ("translation", "rotation", "scale"):
                    keys = ch[kind]
                    if not keys:
                        continue
                    rest = {kind: None}[kind]
                    if kind == "translation":
                        rest = list(BIND_TRANSLATION[ch["joint"]])
                    elif kind == "rotation":
                        rest = [0.0, 0.0, 0.0, 1.0]
                    else:
                        rest = [1.0, 1.0, 1.0]
                    c.check(f"{ctx}/{ch['joint']}/{kind}-returns-to-rest",
                            vec_close(keys[-1]["value"], rest, 1e-6),
                            f"last key {keys[-1]['value']} != rest {rest}")
    c.check("animations/total-keys", total_keys <= 262144, "key budget exceeded")

    # --- geometry sanity -------------------------------------------------
    positions = [v["position"] for v in verts]
    ys = [p[1] for p in positions]
    xs = [p[0] for p in positions]
    zs = [p[2] for p in positions]
    c.check("geometry/feet-on-floor", close(min(ys), 0.0, 1e-4),
            f"lowest vertex at y = {min(ys)}")
    c.check("geometry/height", close(max(ys), EAR_TIP_Y, 5e-3),
            f"highest vertex at y = {max(ys)} (ear tip {EAR_TIP_Y})")
    c.check("geometry/ground-clearance", min(ys) >= -1e-6, "geometry below y = 0")
    c.check("geometry/bounds", max(abs(x) for x in xs) < 0.7 and max(abs(z) for z in zs) < 0.5,
            "silhouette wider than expected")
    # Every shell must be a closed surface: every directed edge appears exactly
    # once and every undirected edge exactly twice (watertight, no holes, no
    # T-junctions), and the Euler characteristic must match the declared
    # topology (2 for a sphere, 0 for a torus).
    if mesh is not None:
        parts = mesh.parts
        c.check("parts/unique-names",
                len(parts) == len(set(p["part"] for p in parts)),
                "duplicate part name")
        c.check("parts/total-triangles-budget", len(mesh.indices) // 3 < 8000,
                "triangle budget exceeded")
        for part in parts:
            name = part["part"]
            start = part["index_start"]
            count = part["triangles"] * 3
            triangles = [
                tuple(mesh.indices[start + i:start + i + 3])
                for i in range(0, count, 3)
            ]
            undirected = {}
            directed = set()
            duplicate_directed = False
            for triangle in triangles:
                for k in range(3):
                    a, b = triangle[k], triangle[(k + 1) % 3]
                    key = (min(a, b), max(a, b))
                    undirected[key] = undirected.get(key, 0) + 1
                    # A consistently wound closed shell uses every undirected
                    # edge once per direction; the same direction twice is the
                    # inconsistency (a flipped neighbour triangle).
                    if (a, b) in directed:
                        duplicate_directed = True
                    directed.add((a, b))
            boundary = [edge for edge, n in undirected.items() if n != 2]
            v_in_part = part["vertices"]
            f = len(triangles)
            e_edges = len(undirected)
            chi = v_in_part - e_edges + f
            genus = (2 - chi) // 2
            c.check(f"parts/{name}/watertight", not boundary,
                    f"{len(boundary)} boundary edges")
            c.check(f"parts/{name}/edge-orientation", not duplicate_directed,
                    "an edge is used twice in the same direction")
            c.check(f"parts/{name}/euler", genus == part["extra"].get("genus", 0),
                    f"Euler characteristic {chi} (genus {genus})")
            c.check(f"parts/{name}/nonempty", part["vertices"] > 0, "empty part")
        c.check("parts/eye-front-of-screen", _eyes_in_front(), "eye/screen clearance")
    return c


def _eyes_in_front():
    """Each eye sphere must straddle the visor surface: embedded far enough
    behind it (no floating gap, no z-fighting) with a bounded visible dome
    (0.012 .. 0.06) so it always reads as a bright round eye."""
    for sign in (1.0, -1.0):
        x = sign * EYE_X
        t = 1.0 - (x / SCREEN_RADII[0]) ** 2 - ((EYE_Y - SCREEN_CENTER[1]) / SCREEN_RADII[1]) ** 2
        if t <= 0.0:
            return False        # the eye is off the visor entirely
        screen_z = SCREEN_CENTER[2] + SCREEN_RADII[2] * math.sqrt(t)
        back = EYE_Z - EYE_RADIUS
        front = EYE_Z + EYE_RADIUS
        if back >= screen_z - 0.02:
            return False        # barely touches: would float or z-fight
        if front <= screen_z:
            return False        # fully hidden inside the visor
        dome = front - screen_z
        if not 0.012 <= dome <= 0.06:
            return False        # hidden, or absurdly proud
    return True


# ---------------------------------------------------------------------------
# Serialization
# ---------------------------------------------------------------------------


def clean(value):
    """Round floats to 6 decimals and drop negative zero."""
    if isinstance(value, float):
        r = round(value, 6)
        return 0.0 if r == 0.0 else r
    if isinstance(value, (list, tuple)):
        return [clean(v) for v in value]
    if isinstance(value, dict):
        return {k: clean(v) for k, v in value.items()}
    return value


def serialize(doc):
    cleaned = clean(doc)
    text = json.dumps(cleaned, ensure_ascii=False, indent=2)
    return text + "\n"


def report_payload(doc, checker, path, text, extra):
    skel = doc["skeleton3d"]
    positions = [v["position"] for v in skel["vertices"]]
    payload = {
        "generated_by": "tools/generate_bangboo.py",
        "generated_utc": datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0).isoformat(),
        "asset_path": str(path),
        "asset_bytes": len(text.encode("utf-8")),
        "asset_sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
        "vertices": len(skel["vertices"]),
        "triangles": len(skel["indices"]) // 3,
        "joints": len(skel["joints"]),
        "animations": [
            {
                "name": a["name"],
                "duration": a["duration"],
                "channels": len(a["channels"]),
                "keys": sum(
                    len(ch["translation"]) + len(ch["rotation"]) + len(ch["scale"])
                    for ch in a["channels"]
                ),
            }
            for a in skel["animations"]
        ],
        "bbox": {
            "min": [min(p[i] for p in positions) for i in range(3)],
            "max": [max(p[i] for p in positions) for i in range(3)],
        },
        "checks": checker.checks,
        "failures": checker.failures,
        "parts": extra.get("parts", []),
    }
    return payload


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def run_checks(doc, mesh, label):
    checker = Checker()
    try:
        self_check(doc, mesh, checker=checker)
    except CheckAbort as abort:
        print(f"[abort] structural check failed: {abort}")
    print(f"[self-check] {label}: {len(checker.checks)} checks, "
          f"{len(checker.failures)} failures")
    for failure in checker.failures:
        print(f"[FAIL] {failure}")
    return checker


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Generate the bangboo example asset (deterministic, stdlib only)."
    )
    root = Path(__file__).resolve().parent.parent
    parser.add_argument("--output", type=Path,
                        default=root / "assets" / "examples" / "bangboo.dmodel.json",
                        help="destination path (default: assets/examples/bangboo.dmodel.json)")
    parser.add_argument("--force", action="store_true",
                        help="overwrite the destination if it already exists "
                             "(only for this block's new asset)")
    parser.add_argument("--report", type=Path, default=None,
                        help="write the self-check report as JSON")
    parser.add_argument("--check", type=Path, default=None,
                        help="verify an existing file instead of generating")
    args = parser.parse_args(argv)

    if args.check is not None:
        try:
            doc = json.loads(args.check.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            print(f"[FAIL] cannot read {args.check}: {error}")
            return 1
        checker = run_checks(doc, None, str(args.check))
        if args.report is not None:
            args.report.write_text(
                json.dumps(report_payload(doc, checker, args.check, "", {}), indent=2) + "\n",
                encoding="utf-8", newline="\n")
        return 1 if checker.failures else 0

    doc, mesh = build_document()
    text = serialize(doc)
    # Checks run on the rounded document: that is what actually lands on disk.
    checker = run_checks(clean(doc), mesh, "generated document")

    # Determinism: a second build must serialize identically.
    doc2, _ = build_document()
    checker.check("reproducible/deterministic", serialize(doc2) == text,
                  "second build differs")

    if checker.failures:
        print("[abort] self-checks failed; nothing written")
        return 1

    destination = args.output
    if destination.exists() and not args.force:
        print(f"[abort] {destination} exists; pass --force to overwrite this asset")
        return 1
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text, encoding="utf-8", newline="\n")

    skel = doc["skeleton3d"]
    positions = [v["position"] for v in skel["vertices"]]
    print(f"[write] {destination}")
    print(f"[write] bytes={len(text.encode('utf-8'))} "
          f"sha256={hashlib.sha256(text.encode('utf-8')).hexdigest()}")
    print(f"[stats] vertices={len(skel['vertices'])} "
          f"triangles={len(skel['indices']) // 3} joints={len(skel['joints'])}")
    print("[bbox] min=({:.3f}, {:.3f}, {:.3f}) max=({:.3f}, {:.3f}, {:.3f})".format(
        *(min(p[i] for p in positions) for i in range(3)),
        *(max(p[i] for p in positions) for i in range(3)),
    ))
    for anim in skel["animations"]:
        keys = sum(len(ch["translation"]) + len(ch["rotation"]) + len(ch["scale"])
                   for ch in anim["channels"])
        print(f"[anim] {anim['name']:<9} duration={anim['duration']:<4} "
              f"channels={len(anim['channels']):<2} keys={keys}")
    if args.report is not None:
        payload = report_payload(doc, checker, destination, text, {"parts": mesh.parts})
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(payload, indent=2) + "\n",
                               encoding="utf-8", newline="\n")
        print(f"[report] {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
