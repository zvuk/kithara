pub(super) type VdspBiquadmSetup = *mut std::ffi::c_void;
pub(super) type VdspDftSetup = *mut std::ffi::c_void;
pub(super) type VdspLength = usize;
pub(super) type VdspStride = isize;

/// One interleaved complex sample: vDSP's `DSPComplex`.
#[repr(C)]
pub(super) struct DspComplex {
    real: f32,
    imag: f32,
}

/// Two planar arrays viewed as split complex data: vDSP's `DSPSplitComplex`.
#[repr(C)]
pub(super) struct DspSplitComplex {
    pub(super) realp: *mut f32,
    pub(super) imagp: *mut f32,
}

#[link(name = "Accelerate", kind = "framework")]
unsafe extern "C" {
    pub(super) fn vDSP_DFT_DestroySetup(setup: VdspDftSetup);

    pub(super) fn vDSP_DFT_Execute(
        setup: *const std::ffi::c_void,
        ir: *const f32,
        ii: *const f32,
        or: *mut f32,
        oi: *mut f32,
    );

    pub(super) fn vDSP_DFT_zrop_CreateSetup(
        previous: VdspDftSetup,
        length: VdspLength,
        direction: i32,
    ) -> VdspDftSetup;

    pub(super) fn vDSP_biquadm(
        setup: VdspBiquadmSetup,
        x: *mut *const f32,
        ix: VdspStride,
        y: *mut *mut f32,
        iy: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_biquadm_CopyState(
        destination: VdspBiquadmSetup,
        source: *const std::ffi::c_void,
    );

    pub(super) fn vDSP_biquadm_CreateSetup(
        coefficients: *const f64,
        sections: VdspLength,
        channels: VdspLength,
    ) -> VdspBiquadmSetup;

    pub(super) fn vDSP_biquadm_DestroySetup(setup: VdspBiquadmSetup);

    pub(super) fn vDSP_biquadm_ResetState(setup: VdspBiquadmSetup);

    pub(super) fn vDSP_biquadm_SetCoefficientsDouble(
        setup: VdspBiquadmSetup,
        coefficients: *const f64,
        start_section: VdspLength,
        start_channel: VdspLength,
        sections: VdspLength,
        channels: VdspLength,
    );

    pub(super) fn vDSP_conv(
        a: *const f32,
        ia: VdspStride,
        f: *const f32,
        filter_stride: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        p: VdspLength,
    );

    pub(super) fn vDSP_ctoz(
        c: *const DspComplex,
        ic: VdspStride,
        split: *const DspSplitComplex,
        split_stride: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_maxmgv(a: *const f32, ia: VdspStride, c: *mut f32, n: VdspLength);

    pub(super) fn vDSP_svesq(a: *const f32, ia: VdspStride, c: *mut f32, n: VdspLength);

    pub(super) fn vDSP_vadd(
        a: *const f32,
        ia: VdspStride,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_vasm(
        a: *const f32,
        ia: VdspStride,
        b: *const f32,
        ib: VdspStride,
        c: *const f32,
        d: *mut f32,
        id: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_vlint(
        a: *const f32,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        m: VdspLength,
    );

    pub(super) fn vDSP_vmul(
        a: *const f32,
        ia: VdspStride,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
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

    pub(super) fn vDSP_vsma(
        a: *const f32,
        ia: VdspStride,
        b: *const f32,
        c: *const f32,
        ic: VdspStride,
        d: *mut f32,
        id: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_vsmsa(
        a: *const f32,
        ia: VdspStride,
        b: *const f32,
        c: *const f32,
        d: *mut f32,
        id: VdspStride,
        n: VdspLength,
    );

    /// `c = a - b`, with the subtrahend first, as vDSP declares it.
    pub(super) fn vDSP_vsub(
        b: *const f32,
        ib: VdspStride,
        a: *const f32,
        ia: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
    );

    pub(super) fn vDSP_ztoc(
        split: *const DspSplitComplex,
        split_stride: VdspStride,
        c: *mut DspComplex,
        ic: VdspStride,
        n: VdspLength,
    );
}
