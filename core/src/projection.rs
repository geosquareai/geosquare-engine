//! Map projections for grid CRS profiles (spec 2.0, section 4.1).
//!
//! The 11 spec 2.0 profiles use two projections: Lambert Conic Conformal (2SP) and
//! Transverse Mercator (Singapore only). Both are written here in pure Rust, with no
//! native dependency, so the same code runs in Python, WebAssembly and the server.
//!
//! Angles are in degrees, and projected coordinates are in metres. The reference is PROJ
//! 9.5.1 (via `pyproj` 3.7.1). This module reproduces the projection step of that reference
//! exactly. PROJ also applies a datum shift from WGS 84 for some domains, and this module
//! does not. [`crate::domain::CRS_DATUM_SHIFT_UNDECLARED`] lists those domains, and
//! `tests/crs_points.rs` checks the rest.
//!
//! Inputs must be finite. The functions do not check ranges. Callers check them.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

/// Reference ellipsoid, given by semi-major axis and inverse flattening.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipsoid {
    pub semi_major_m: f64,
    pub inverse_flattening: f64,
}

impl Ellipsoid {
    pub const fn new(semi_major_m: f64, inverse_flattening: f64) -> Self {
        Ellipsoid {
            semi_major_m,
            inverse_flattening,
        }
    }

    fn flattening(&self) -> f64 {
        1.0 / self.inverse_flattening
    }

    /// First eccentricity squared.
    fn e2(&self) -> f64 {
        let f = self.flattening();
        f * (2.0 - f)
    }
}

/// A projection and its parameters. Angles are in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Lambert Conic Conformal with two standard parallels.
    LambertConformalConic2Sp {
        lat_origin: f64,
        lon_origin: f64,
        lat_1: f64,
        lat_2: f64,
    },
    /// Transverse Mercator. The series is Krüger's, to sixth order in the third flattening.
    TransverseMercator {
        lat_origin: f64,
        lon_origin: f64,
        scale: f64,
    },
}

/// A projected CRS: an ellipsoid, a projection, and the false origin offsets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridCrs {
    pub ellipsoid: Ellipsoid,
    pub projection: Projection,
    pub false_easting_m: f64,
    pub false_northing_m: f64,
}

impl GridCrs {
    /// Project a longitude and latitude in degrees to metres.
    ///
    /// Returns `None` where the projection is undefined. For a transverse Mercator that is
    /// more than 90 degrees of longitude from the central meridian.
    pub fn forward(&self, lon_deg: f64, lat_deg: f64) -> Option<(f64, f64)> {
        let lon = lon_deg.to_radians();
        let lat = lat_deg.to_radians();
        let (x, y) = match self.projection {
            Projection::LambertConformalConic2Sp {
                lat_origin,
                lon_origin,
                lat_1,
                lat_2,
            } => lcc_forward(
                &self.ellipsoid,
                [lat_1, lat_2, lat_origin].map(f64::to_radians),
                lon_origin.to_radians(),
                lat,
                lon,
            ),
            Projection::TransverseMercator {
                lat_origin,
                lon_origin,
                scale,
            } => {
                let dlon = wrap_radians(lon - lon_origin.to_radians());
                if dlon.abs() > FRAC_PI_2 {
                    return None;
                }
                tm_forward(
                    &self.ellipsoid,
                    lat_origin.to_radians(),
                    lon_origin.to_radians(),
                    scale,
                    lat,
                    lon,
                )
            }
        };
        let (x, y) = (x + self.false_easting_m, y + self.false_northing_m);
        (x.is_finite() && y.is_finite()).then_some((x, y))
    }

    /// Unproject metres to `(lon, lat)` in degrees. Longitude is wrapped to -180..180.
    ///
    /// Returns `None` if the result is not a finite point on the globe.
    pub fn inverse(&self, x_m: f64, y_m: f64) -> Option<(f64, f64)> {
        let x = x_m - self.false_easting_m;
        let y = y_m - self.false_northing_m;
        let (lon, lat) = match self.projection {
            Projection::LambertConformalConic2Sp {
                lat_origin,
                lon_origin,
                lat_1,
                lat_2,
            } => lcc_inverse(
                &self.ellipsoid,
                [lat_1, lat_2, lat_origin].map(f64::to_radians),
                lon_origin.to_radians(),
                x,
                y,
            ),
            Projection::TransverseMercator {
                lat_origin,
                lon_origin,
                scale,
            } => tm_inverse(
                &self.ellipsoid,
                lat_origin.to_radians(),
                lon_origin.to_radians(),
                scale,
                x,
                y,
            ),
        };
        let lon_deg = wrap_radians(lon).to_degrees();
        let lat_deg = lat.to_degrees();
        let valid = lon.is_finite() && lat.is_finite() && lat.abs() <= FRAC_PI_2;
        valid.then_some((lon_deg, lat_deg))
    }
}

/// Wrap an angle in radians to the range -pi..=pi.
fn wrap_radians(angle: f64) -> f64 {
    let wrapped = (angle + PI).rem_euclid(TAU) - PI;
    if wrapped.is_finite() {
        wrapped
    } else {
        angle
    }
}

// ---------------------------------------------------------------------------
// Lambert Conic Conformal (2SP), Snyder (1987) section 15.
// ---------------------------------------------------------------------------

/// Snyder's `m`: cos(phi) / sqrt(1 - e^2 sin^2(phi)).
fn lcc_m(e2: f64, phi: f64) -> f64 {
    phi.cos() / (1.0 - e2 * phi.sin().powi(2)).sqrt()
}

/// Snyder's `t`: tan(pi/4 - phi/2) / ((1 - e sin phi) / (1 + e sin phi))^(e/2).
fn lcc_t(e: f64, phi: f64) -> f64 {
    let s = phi.sin();
    (FRAC_PI_4 - phi / 2.0).tan() / ((1.0 - e * s) / (1.0 + e * s)).powf(e / 2.0)
}

/// Cone constants `(n, F)` for the two standard parallels.
fn lcc_cone(e: f64, e2: f64, lat_1: f64, lat_2: f64) -> (f64, f64) {
    let m1 = lcc_m(e2, lat_1);
    let t1 = lcc_t(e, lat_1);
    let n = if (lat_1 - lat_2).abs() < 1e-12 {
        lat_1.sin()
    } else {
        let m2 = lcc_m(e2, lat_2);
        let t2 = lcc_t(e, lat_2);
        (m1.ln() - m2.ln()) / (t1.ln() - t2.ln())
    };
    let f = m1 / (n * t1.powf(n));
    (n, f)
}

fn lcc_forward(
    ellipsoid: &Ellipsoid,
    [lat_1, lat_2, lat_0]: [f64; 3],
    lon_0: f64,
    lat: f64,
    lon: f64,
) -> (f64, f64) {
    let a = ellipsoid.semi_major_m;
    let e2 = ellipsoid.e2();
    let e = e2.sqrt();
    let (n, f) = lcc_cone(e, e2, lat_1, lat_2);
    let rho = |phi: f64| a * f * lcc_t(e, phi).powf(n);
    let rho_0 = rho(lat_0);
    let theta = n * wrap_radians(lon - lon_0);
    let r = rho(lat);
    (r * theta.sin(), rho_0 - r * theta.cos())
}

fn lcc_inverse(
    ellipsoid: &Ellipsoid,
    [lat_1, lat_2, lat_0]: [f64; 3],
    lon_0: f64,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let a = ellipsoid.semi_major_m;
    let e2 = ellipsoid.e2();
    let e = e2.sqrt();
    let (n, f) = lcc_cone(e, e2, lat_1, lat_2);
    let rho_0 = a * f * lcc_t(e, lat_0).powf(n);
    // Signed radius and angle. The sign of `n` decides which side of the apex we are on.
    let sign = n.signum();
    let dy = rho_0 - y;
    let rho = sign * x.hypot(dy);
    let theta = (sign * x).atan2(sign * dy);
    let t = (rho / (a * f)).powf(1.0 / n);

    // Solve t = tan(pi/4 - phi/2) / ((1 - e sin phi)/(1 + e sin phi))^(e/2) for phi.
    let mut phi = FRAC_PI_2 - 2.0 * t.atan();
    for _ in 0..30 {
        let next = FRAC_PI_2
            - 2.0 * (t * ((1.0 - e * phi.sin()) / (1.0 + e * phi.sin())).powf(e / 2.0)).atan();
        let converged = (next - phi).abs() < 1e-15;
        phi = next;
        if converged {
            break;
        }
    }
    (lon_0 + theta / n, phi)
}

// ---------------------------------------------------------------------------
// Transverse Mercator, Krüger series to sixth order (Karney 2011).
// ---------------------------------------------------------------------------

/// Krüger `alpha` coefficients for the forward series, in powers of the third flattening.
fn tm_alpha(n: f64) -> [f64; 6] {
    let n2 = n * n;
    let n3 = n2 * n;
    let n4 = n3 * n;
    let n5 = n4 * n;
    let n6 = n5 * n;
    [
        n / 2.0 - 2.0 * n2 / 3.0 + 5.0 * n3 / 16.0 + 41.0 * n4 / 180.0 - 127.0 * n5 / 288.0
            + 7891.0 * n6 / 37800.0,
        13.0 * n2 / 48.0 - 3.0 * n3 / 5.0 + 557.0 * n4 / 1440.0 + 281.0 * n5 / 630.0
            - 1983433.0 * n6 / 1935360.0,
        61.0 * n3 / 240.0 - 103.0 * n4 / 140.0 + 15061.0 * n5 / 26880.0 + 167603.0 * n6 / 181440.0,
        49561.0 * n4 / 161280.0 - 179.0 * n5 / 168.0 + 6601661.0 * n6 / 7257600.0,
        34729.0 * n5 / 80640.0 - 3418889.0 * n6 / 1995840.0,
        212378941.0 * n6 / 319334400.0,
    ]
}

/// Krüger `beta` coefficients for the inverse series, in powers of the third flattening.
fn tm_beta(n: f64) -> [f64; 6] {
    let n2 = n * n;
    let n3 = n2 * n;
    let n4 = n3 * n;
    let n5 = n4 * n;
    let n6 = n5 * n;
    [
        n / 2.0 - 2.0 * n2 / 3.0 + 37.0 * n3 / 96.0 - n4 / 360.0 - 81.0 * n5 / 512.0
            + 96199.0 * n6 / 604800.0,
        n2 / 48.0 + n3 / 15.0 - 437.0 * n4 / 1440.0 + 46.0 * n5 / 105.0
            - 1118711.0 * n6 / 3870720.0,
        17.0 * n3 / 480.0 - 37.0 * n4 / 840.0 - 209.0 * n5 / 4480.0 + 5569.0 * n6 / 90720.0,
        4397.0 * n4 / 161280.0 - 11.0 * n5 / 504.0 - 830251.0 * n6 / 7257600.0,
        4583.0 * n5 / 161280.0 - 108847.0 * n6 / 3991680.0,
        20648693.0 * n6 / 638668800.0,
    ]
}

/// Map a latitude to the auxiliary tau' used by the Krüger series.
fn tm_tau_prime(e: f64, tau: f64) -> f64 {
    let sigma = (e * (e * tau / (1.0 + tau * tau).sqrt()).atanh()).sinh();
    tau * (1.0 + sigma * sigma).sqrt() - sigma * (1.0 + tau * tau).sqrt()
}

/// Krüger's conformal coordinates `(xi, eta)` for a latitude and a longitude offset.
/// Both are dimensionless and scale with the meridian radius `A`.
fn tm_xi_eta(e: f64, n: f64, lat: f64, dlon: f64) -> (f64, f64) {
    let tau_p = tm_tau_prime(e, lat.tan());
    let xi_p = tau_p.atan2(dlon.cos());
    let eta_p = (dlon.sin() / (tau_p * tau_p + dlon.cos().powi(2)).sqrt()).asinh();

    let alpha = tm_alpha(n);
    let (mut xi, mut eta) = (xi_p, eta_p);
    for (j, a) in alpha.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi += a * (k * xi_p).sin() * (k * eta_p).cosh();
        eta += a * (k * xi_p).cos() * (k * eta_p).sinh();
    }
    (xi, eta)
}

/// Meridian radius constant `A` of the Krüger series, in metres.
fn tm_big_a(ellipsoid: &Ellipsoid, n: f64) -> f64 {
    ellipsoid.semi_major_m / (1.0 + n)
        * (1.0 + n.powi(2) / 4.0 + n.powi(4) / 64.0 + n.powi(6) / 256.0)
}

fn tm_forward(
    ellipsoid: &Ellipsoid,
    lat_0: f64,
    lon_0: f64,
    scale: f64,
    lat: f64,
    lon: f64,
) -> (f64, f64) {
    let n = ellipsoid.flattening() / (2.0 - ellipsoid.flattening());
    let e = ellipsoid.e2().sqrt();
    let big_a = tm_big_a(ellipsoid, n);

    let (xi, eta) = tm_xi_eta(e, n, lat, lon - lon_0);
    // Northing is measured from the meridian arc through the origin latitude.
    let (xi_0, _) = tm_xi_eta(e, n, lat_0, 0.0);
    (scale * big_a * eta, scale * big_a * (xi - xi_0))
}

fn tm_inverse(
    ellipsoid: &Ellipsoid,
    lat_0: f64,
    lon_0: f64,
    scale: f64,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let n = ellipsoid.flattening() / (2.0 - ellipsoid.flattening());
    let e2 = ellipsoid.e2();
    let e = e2.sqrt();
    let big_a = tm_big_a(ellipsoid, n);

    let (xi_0, _) = tm_xi_eta(e, n, lat_0, 0.0);
    let xi = y / (scale * big_a) + xi_0;
    let eta = x / (scale * big_a);
    let beta = tm_beta(n);
    let (mut xi_p, mut eta_p) = (xi, eta);
    for (j, b) in beta.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi_p -= b * (k * xi).sin() * (k * eta).cosh();
        eta_p -= b * (k * xi).cos() * (k * eta).sinh();
    }

    let tau_p = xi_p.sin() / (eta_p.sinh().powi(2) + xi_p.cos().powi(2)).sqrt();
    let dlon = eta_p.sinh().atan2(xi_p.cos());

    // Newton iteration on tau: tau' is a monotonic function of tau.
    let mut tau = tau_p;
    for _ in 0..50 {
        let tau_p_i = tm_tau_prime(e, tau);
        let step = (tau_p - tau_p_i) * (1.0 + (1.0 - e2) * tau * tau)
            / ((1.0 - e2) * (1.0 + tau * tau).sqrt() * (1.0 + tau_p_i * tau_p_i).sqrt());
        tau += step;
        if step.abs() < 1e-15 {
            break;
        }
    }
    (lon_0 + dlon, tau.atan())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WGS84: Ellipsoid = Ellipsoid::new(6_378_137.0, 298.257_223_563);

    fn lcc_id() -> GridCrs {
        GridCrs {
            ellipsoid: WGS84,
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: -2.550_295_649_999_98,
                lon_origin: 118.015_214_45,
                lat_1: -8.188_649_283_333_32,
                lat_2: 3.088_057_983_333_35,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        }
    }

    fn tm_sg() -> GridCrs {
        GridCrs {
            ellipsoid: WGS84,
            projection: Projection::TransverseMercator {
                lat_origin: 1.3299809,
                lon_origin: 103.8469117,
                scale: 1.0,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        }
    }

    #[test]
    fn natural_origin_maps_to_zero() {
        let (x, y) = lcc_id()
            .forward(118.015_214_45, -2.550_295_649_999_98)
            .unwrap();
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6, "({x}, {y})");
        let (x, y) = tm_sg().forward(103.8469117, 1.3299809).unwrap();
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6, "({x}, {y})");
    }

    #[test]
    fn lcc_round_trips_across_a_domain() {
        let crs = lcc_id();
        for lon in [95.0, 106.8456, 118.0, 130.0, 141.0] {
            for lat in [-11.0, -6.2088, -2.5, 0.114714, 5.0] {
                let (x, y) = crs.forward(lon, lat).unwrap();
                let (lon2, lat2) = crs.inverse(x, y).unwrap();
                assert!((lon2 - lon).abs() < 1e-10, "lon {lon} {lat}: {lon2}");
                assert!((lat2 - lat).abs() < 1e-10, "lat {lon} {lat}: {lat2}");
            }
        }
    }

    #[test]
    fn transverse_mercator_round_trips_far_from_the_central_meridian() {
        let crs = tm_sg();
        for lon in [102.0, 103.5, 103.8469117, 104.5, 106.0] {
            for lat in [-2.0, 0.5, 1.33, 3.0] {
                let (x, y) = crs.forward(lon, lat).unwrap();
                let (lon2, lat2) = crs.inverse(x, y).unwrap();
                assert!((lon2 - lon).abs() < 1e-10, "lon {lon} {lat}: {lon2}");
                assert!((lat2 - lat).abs() < 1e-10, "lat {lon} {lat}: {lat2}");
            }
        }
    }

    #[test]
    fn transverse_mercator_matches_pyproj_northing_from_the_origin_latitude() {
        // On the central meridian x is zero. The northing is the meridian arc from the
        // origin latitude 1.3299809 to 2.0 degrees. pyproj 3.7.1 (PROJ 9.5.1) gives 74087.5133.
        let (x, y) = tm_sg().forward(103.8469117, 2.0).unwrap();
        assert!(x.abs() < 1e-6, "{x}");
        assert!((y - 74_087.513_294).abs() < 1e-3, "{y}");
    }

    #[test]
    fn transverse_mercator_is_undefined_beyond_ninety_degrees_of_longitude() {
        assert!(tm_sg().forward(103.8469117 + 91.0, 1.0).is_none());
        assert!(tm_sg().forward(103.8469117 - 89.0, 1.0).is_some());
    }

    #[test]
    fn lambert_longitude_wraps_at_the_antimeridian() {
        let crs = lcc_id();
        let (x1, y1) = crs.forward(118.0, -2.0).unwrap();
        let (x2, y2) = crs.forward(118.0 + 360.0, -2.0).unwrap();
        assert!((x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6);
    }
}
