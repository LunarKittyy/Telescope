//! The eight ways a picture can be turned, as small matrices, so turns can be added up without touching pixels.

use libjpeg_turbo_rs::TransformOp;

const ALL: [TransformOp; 8] = [
    TransformOp::None, TransformOp::HFlip, TransformOp::VFlip, TransformOp::Rot180,
    TransformOp::Rot90, TransformOp::Rot270, TransformOp::Transpose, TransformOp::Transverse,
];

type Matrix = [[i8; 2]; 2];

/// Where a point goes, with x to the right and y down.
fn matrix(op: TransformOp) -> Matrix {
    match op {
        TransformOp::None => [[1, 0], [0, 1]],
        TransformOp::HFlip => [[-1, 0], [0, 1]],
        TransformOp::VFlip => [[1, 0], [0, -1]],
        TransformOp::Rot180 => [[-1, 0], [0, -1]],
        TransformOp::Rot90 => [[0, -1], [1, 0]],
        TransformOp::Rot270 => [[0, 1], [-1, 0]],
        TransformOp::Transpose => [[0, 1], [1, 0]],
        TransformOp::Transverse => [[0, -1], [-1, 0]],
    }
}

/// What turning by `first` and then by `second` comes to.
pub fn then(first: TransformOp, second: TransformOp) -> TransformOp {
    let (a, b) = (matrix(second), matrix(first));
    let product = [0, 1].map(|row| [0, 1].map(|col| a[row][0] * b[0][col] + a[row][1] * b[1][col]));
    ALL.into_iter().find(|op| matrix(*op) == product).unwrap_or(TransformOp::None)
}

/// The EXIF orientation value (1 to 8) that shows a picture turned this way.
pub fn exif_value(op: TransformOp) -> u16 {
    match op {
        TransformOp::None => 1,
        TransformOp::HFlip => 2,
        TransformOp::Rot180 => 3,
        TransformOp::VFlip => 4,
        TransformOp::Transpose => 5,
        TransformOp::Rot90 => 6,
        TransformOp::Transverse => 7,
        TransformOp::Rot270 => 8,
    }
}

/// A turn as a rotation clockwise in quarter turns, then a left-right flip: how HEIF writes it (`irot`, then `imir`).
pub fn rotate_then_mirror(op: TransformOp) -> (u8, bool) {
    for quarters in 0..4u8 {
        let turn = [TransformOp::None, TransformOp::Rot90, TransformOp::Rot180, TransformOp::Rot270][usize::from(quarters)];
        for mirror in [false, true] {
            if then(turn, if mirror { TransformOp::HFlip } else { TransformOp::None }) == op {
                return (quarters, mirror);
            }
        }
    }
    (0, false)
}
