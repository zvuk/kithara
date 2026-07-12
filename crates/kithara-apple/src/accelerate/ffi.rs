pub(super) type VdspBiquadSetup = *mut std::ffi::c_void;
pub(super) type VdspLength = usize;
pub(super) type VdspStride = isize;

#[link(name = "Accelerate", kind = "framework")]
unsafe extern "C" {
    pub(super) fn cblas_scopy(n: i32, x: *const f32, inc_x: i32, y: *mut f32, inc_y: i32);

    pub(super) fn vDSP_biquad(
        setup: VdspBiquadSetup,
        delay: *mut f32,
        x: *const f32,
        ix: VdspStride,
        y: *mut f32,
        iy: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_biquad_CreateSetup(
        coefficients: *const f64,
        order: VdspLength,
    ) -> VdspBiquadSetup;

    pub(super) fn vDSP_biquad_DestroySetup(setup: VdspBiquadSetup);

    pub(super) fn vDSP_vclr(c: *mut f32, ic: VdspStride, n: VdspLength);

    pub(super) fn vDSP_vlint(
        a: *const f32,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        m: VdspLength,
    );

    pub(super) fn vDSP_vqint(
        a: *const f32,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        m: VdspLength,
    );

    pub(super) fn vDSP_vramp(
        a: *const f32,
        b: *const f32,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
    );
}
