//! One Robertson/CIE-1960 model shared with native/white_balance.h. The table
//! is included from the same source, so native sliders and the grey picker agree.
use std::sync::OnceLock;
pub type Matrix = [[f64; 3]; 3];
fn table() -> &'static [[f64; 4]] {
    static TABLE: OnceLock<Vec<[f64; 4]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../native/white_balance_table.inc")
            .split('{')
            .skip(1)
            .map(|row| {
                let mut values = row
                    .split('}')
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|v| v.trim().parse::<f64>().expect("compiled Robertson datum"));
                std::array::from_fn(|_| values.next().expect("four isotherm fields"))
            })
            .collect()
    })
}
fn direction(row: [f64; 4]) -> [f64; 2] {
    let norm = (1. + row[3] * row[3]).sqrt();
    [1. / norm, row[3] / norm]
}
pub fn xyz(kelvin: f64, tint: f64) -> Option<[f64; 3]> {
    if !kelvin.is_finite()
        || !tint.is_finite()
        || !(2000. ..=50000.).contains(&kelvin)
        || tint.abs() > 150.
    {
        return None;
    }
    let r = 1e6 / kelvin;
    let data = table();
    let i = (0..data.len() - 1).find(|&i| r < data[i + 1][0] || i == data.len() - 2)?;
    let (a, b) = (data[i], data[i + 1]);
    let w = (b[0] - r) / (b[0] - a[0]);
    let da = direction(a);
    let db = direction(b);
    let d = [w * da[0] + (1. - w) * db[0], w * da[1] + (1. - w) * db[1]];
    let norm = d[0].hypot(d[1]);
    let u = w * a[1] + (1. - w) * b[1] - tint * d[0] / (3000. * norm);
    let v = w * a[2] + (1. - w) * b[2] - tint * d[1] / (3000. * norm);
    let den = 2. * u - 8. * v + 4.;
    let x = 3. * u / den;
    let y = 2. * v / den;
    (x > 0. && y > 0. && x + y < 1.).then_some([x / y, 1., (1. - x - y) / y])
}
pub fn temperature_tint(xyz: [f64; 3]) -> Option<(f64, f64)> {
    if xyz.iter().any(|x| !x.is_finite() || *x <= 0.) {
        return None;
    }
    let den = xyz[0] + 15. * xyz[1] + 3. * xyz[2];
    let u = 4. * xyz[0] / den;
    let v = 6. * xyz[1] / den;
    let data = table();
    let (mut last_distance, mut last_du, mut last_dv) = (0., 0., 0.);
    for i in 1..data.len() {
        let row = data[i];
        let [mut du, mut dv] = direction(row);
        let mut distance = -(u - row[1]) * dv + (v - row[2]) * du;
        if distance <= 0. || i == data.len() - 1 {
            distance = (-distance).max(0.);
            let w = if i == 1 {
                0.
            } else {
                distance / (last_distance + distance)
            };
            let prev = data[i - 1];
            let kelvin = 1e6 / (w * prev[0] + (1. - w) * row[0]);
            let delta = [
                u - (w * prev[1] + (1. - w) * row[1]),
                v - (w * prev[2] + (1. - w) * row[2]),
            ];
            du = w * last_du + (1. - w) * du;
            dv = w * last_dv + (1. - w) * dv;
            let tint = -3000. * (delta[0] * du + delta[1] * dv) / du.hypot(dv);
            return (kelvin.is_finite()
                && tint.is_finite()
                && (1999. ..=50001.).contains(&kelvin)
                && tint.abs() <= 150.1)
                .then_some((kelvin.clamp(2000., 50000.), tint.clamp(-150., 150.)));
        }
        last_distance = distance;
        last_du = du;
        last_dv = dv;
    }
    None
}
pub fn mul(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}
pub fn apply(a: Matrix, b: [f64; 3]) -> [f64; 3] {
    a.map(|row| (0..3).map(|k| row[k] * b[k]).sum())
}
pub fn inverse(a: Matrix) -> Option<Matrix> {
    let mut aug: [[f64; 6]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            if j < 3 {
                a[i][j]
            } else if j - 3 == i {
                1.
            } else {
                0.
            }
        })
    });
    for c in 0..3 {
        let p = (c..3).max_by(|&i, &j| aug[i][c].abs().total_cmp(&aug[j][c].abs()))?;
        if !aug[p][c].is_finite() || aug[p][c].abs() < 1e-12 {
            return None;
        }
        aug.swap(p, c);
        let divisor = aug[c][c];
        for x in &mut aug[c] {
            *x /= divisor;
        }
        let pivot = aug[c];
        for (i, row) in aug.iter_mut().enumerate() {
            if i != c {
                let f = row[c];
                for j in 0..6 {
                    row[j] -= f * pivot[j];
                }
            }
        }
    }
    Some(aug.map(|row| [row[3], row[4], row[5]]))
}
pub fn diagonal(x: [f64; 3]) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| if i == j { x[i] } else { 0. }))
}
pub fn interpolate(a: Matrix, b: Matrix, w: f64) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][j] * w + b[i][j] * (1. - w)))
}
pub const IDENTITY: Matrix = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
pub const D50: [f64; 3] = [0.9642, 1., 0.8251];
pub const D65: [f64; 3] = [0.95047, 1., 1.08883];
pub fn adapt(from: [f64; 3], to: [f64; 3]) -> Option<Matrix> {
    let b = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let src = apply(b, from);
    let dst = apply(b, to);
    if src.iter().any(|x| x.abs() < 1e-10) {
        return None;
    }
    Some(mul(
        mul(
            inverse(b)?,
            diagonal(std::array::from_fn(|i| dst[i] / src[i])),
        ),
        b,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_white_model_and_inverse_agree() {
        assert_eq!(table().len(), 31);
        for t in [2000., 3000., 4850., 5500., 6500., 15000., 50000.] {
            for tint in [-30., 0., 30.] {
                let p = xyz(t, tint).unwrap();
                let (actual_t, actual_tint) = temperature_tint(p).unwrap();
                assert!(
                    (actual_t - t).abs() < 3.,
                    "{t}/{tint}: {actual_t}/{actual_tint}"
                );
                assert!((actual_tint - tint).abs() < 0.03);
            }
        }
    }
    #[test]
    fn bradford_maps_white_and_matrix_inverse_rejects_singular() {
        let white = apply(adapt(D65, D50).unwrap(), D65);
        for i in 0..3 {
            assert!((white[i] - D50[i]).abs() < 1e-10);
        }
        assert!(inverse([[0.; 3]; 3]).is_none());
        let m = [[2., 0.1, 0.3], [0.2, 1., 0.2], [0.1, 0.3, 1.1]];
        let unity = mul(m, inverse(m).unwrap());
        for i in 0..3 {
            for j in 0..3 {
                assert!((unity[i][j] - IDENTITY[i][j]).abs() < 1e-10);
            }
        }
    }
}
