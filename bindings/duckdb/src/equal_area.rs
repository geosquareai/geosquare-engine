//! Equal Earth (EPSG:8857, "WGS 84 / Equal Earth Greenwich"), the equal-area CRS of the
//! spec 2.0 profiles, for `coverage_mode := 'EQUAL_AREA'`.
//!
//! Ellipsoidal form as in PROJ `eqearth`: geodetic latitude to authalic latitude, then
//! the Šavrič et al. (2018) polynomial, scaled by the authalic radius. Coverage ratios
//! only need relative areas, but the full projection is kept for clarity.

const A1: f64 = 1.340264;
const A2: f64 = -0.081106;
const A3: f64 = 0.000893;
const A4: f64 = 0.003796;
const M: f64 = 0.866_025_403_784_438_6; // sqrt(3) / 2

const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;

fn qsfn(sinphi: f64, e: f64, one_es: f64) -> f64 {
    let con = e * sinphi;
    let div1 = 1.0 - con * con;
    let div2 = 1.0 + con;
    one_es * (sinphi / div1 - (0.5 / e) * ((1.0 - con) / div2).ln())
}

/// Forward Equal Earth on WGS84: (lon, lat) degrees → (x, y) metres.
pub fn forward(lon_deg: f64, lat_deg: f64) -> (f64, f64) {
    let es = WGS84_F * (2.0 - WGS84_F);
    let e = es.sqrt();
    let one_es = 1.0 - es;
    let qp = qsfn(1.0, e, one_es);
    let rqda = (0.5 * qp).sqrt();
    let lam = lon_deg.to_radians();
    let sbeta = (qsfn(lat_deg.to_radians().sin(), e, one_es) / qp).clamp(-1.0, 1.0);
    let psi = (M * sbeta).asin();
    let psi2 = psi * psi;
    let psi6 = psi2 * psi2 * psi2;
    let x = lam * psi.cos() / (M * (A1 + 3.0 * A2 * psi2 + psi6 * (7.0 * A3 + 9.0 * A4 * psi2)));
    let y = psi * (A1 + A2 * psi2 + psi6 * (A3 + A4 * psi2));
    (x * rqda * WGS84_A, y * rqda * WGS84_A)
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_proj() {
        // pyproj: Transformer.from_crs("EPSG:4326", "EPSG:8857", always_xy=True)
        let (x, y) = super::forward(106.8456, -6.2088);
        assert!(
            (x - 10_207_345.460_265).abs() < 1e-3 && (y - -796_971.940_350).abs() < 1e-3,
            "{x} {y}"
        );
    }
}
