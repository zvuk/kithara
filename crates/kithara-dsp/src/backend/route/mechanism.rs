#[derive(Clone, Copy, Debug)]
pub(crate) enum Route {
    Platform,
    Simd,
    Scalar,
}

impl Route {
    pub(super) const fn select<F: Copy>(self, mut forms: &[(Self, Option<F>)]) -> Option<F> {
        while let Some((&(route, form), remaining)) = forms.split_first() {
            if matches!(
                (self, route),
                (Self::Platform, Self::Platform)
                    | (Self::Simd, Self::Simd)
                    | (Self::Scalar, Self::Scalar)
            ) {
                return form;
            }
            forms = remaining;
        }
        None
    }

    #[cfg(any(test, feature = "routes"))]
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Simd => "simd",
            Self::Scalar => "scalar",
        }
    }
}

macro_rules! route {
    ($($name:ident)::+ => ($($argument:expr),* $(,)?)) => {
        $($name)::+::invoke($($argument),*)
    };
}

macro_rules! routes {
    (@export $($name:ident[$($generic:tt)+]($($argument:ident: $ty:ty),* $(,)?) -> $result:ty {
        simd: $simd:path;
        scalar: $scalar:path;
        $(platform: $platform:path;)?
        entry: $entry:path;
        targets: [$apple:ident, $x86:ident, $other:ident];
    })+) => {
        $( $crate::backend::route::mechanism::routes! { @generic $name[$($generic)+]($($argument: $ty),*) -> $result {
            simd: $simd; scalar: $scalar; $(platform: $platform;)? entry: $entry; targets: [$apple, $x86, $other];
        } } )+
    };
    (@export $($declarations:tt)+) => {
        $crate::backend::route::mechanism::routes! { $($declarations)+ }
        #[cfg(feature = "routes")]
        pub mod measured {
            $crate::backend::route::mechanism::routes! { @public $($declarations)+ }
        }
    };
    (@public $($name:ident($($argument:ident: $ty:ty),* $(,)?) -> $result:ty {
        simd: $simd:path;
        $(scalar: $scalar:path;)?
        $(platform: $platform:path;)?
        entry: $entry:path;
        targets: [$apple:ident, $x86:ident, $other:ident];
    })+) => {$(
        #[doc = concat!("Execution forms for `", stringify!($name), "`.")]
        pub mod $name {
            /// Function signature shared by this kernel's measured forms.
            pub type Kernel = $crate::backend::route::table::$name::Kernel;

            /// Enumerate available forms and the public entry without allocation.
            pub fn routes() -> impl Iterator<Item = (&'static str, Kernel)> {
                $crate::backend::route::table::$name::routes()
            }
        }
    )+};
    (@public $($name:ident[$($generic:tt)+]($($argument:ident: $ty:ty),* $(,)?) -> $result:ty {
        simd: $simd:path;
        scalar: $scalar:path;
        $(platform: $platform:path;)?
        entry: $entry:path;
        targets: [$apple:ident, $x86:ident, $other:ident];
    })+) => {};
    (@generic $name:ident[$($generic:tt)+]($($argument:ident: $ty:ty),* $(,)?) -> $result:ty {
        simd: $simd:path; scalar: $scalar:path; $(platform: $platform:path;)? entry: $entry:path; targets: [$apple:ident, $x86:ident, $other:ident];
    }) => {
        #[doc = concat!("Execution forms for `", stringify!($name), "`.")]
        pub(crate) mod $name {
            use $crate::backend::route::Route;

            const ROUTE: Route = if cfg!(all(target_vendor = "apple", target_arch = "aarch64")) {
                Route::$apple
            } else if cfg!(target_arch = "x86_64") {
                Route::$x86
            } else {
                Route::$other
            };

            // Unavailable selected cells fail const evaluation, before audio processing (== mono).
            const CHECK: () = {
                let available = $crate::backend::route::mechanism::routes!(@check_match $($platform)?);
                assert!(available, "selected kernel form is unavailable on this target");
            };

            #[inline(always)]
            pub(crate) fn invoke<$($generic)+>($($argument: $ty),*) -> $result {
                let () = CHECK;
                $crate::backend::route::mechanism::routes!(@dispatch_match ($($argument: $ty),*) ; $simd ; $scalar ; $($platform)?)
            }
        }
    };
    (@check_match $platform:path) => {
        match ROUTE {
            Route::Simd => true,
            Route::Scalar => true,
            Route::Platform => cfg!(any(target_os = "macos", target_os = "ios")),
        }
    };
    (@check_match) => {
        match ROUTE {
            Route::Simd => true,
            Route::Scalar => true,
            _ => false,
        }
    };
    (@dispatch_match ($($argument:ident: $ty:ty),*) ; $simd:path ; $scalar:path ; $platform:path) => {{
        let route = ROUTE;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if matches!(route, Route::Platform) {
            return $platform($($argument),*);
        }
        if matches!(route, Route::Simd) {
            $simd($($argument),*)
        } else {
            $scalar($($argument),*)
        }
    }};
    (@dispatch_match ($($argument:ident: $ty:ty),*) ; $simd:path ; $scalar:path ;) => {{
        let route = ROUTE;
        if matches!(route, Route::Simd) {
            $simd($($argument),*)
        } else {
            $scalar($($argument),*)
        }
    }};

    ($($name:ident($($argument:ident: $ty:ty),* $(,)?) -> $result:ty {
        simd: $simd:path;
        $(scalar: $scalar:path;)?
        $(platform: $platform:path;)?
        entry: $entry:path;
        targets: [$apple:ident, $x86:ident, $other:ident];
    })+) => {$(
        #[doc = concat!("Execution forms for `", stringify!($name), "`.")]
        pub(crate) mod $name {
            use $crate::backend::route::Route;

            pub(crate) type Kernel = fn($($ty),*) -> $result;

            #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
            const ROUTE: Route = Route::$apple;
            #[cfg(target_arch = "x86_64")]
            const ROUTE: Route = Route::$x86;
            #[cfg(not(any(all(target_vendor = "apple", target_arch = "aarch64"), target_arch = "x86_64")))]
            const ROUTE: Route = Route::$other;

            const FORMS: &[(Route, Option<Kernel>)] = &[
                (Route::Platform, $crate::backend::route::mechanism::routes!(@platform $($platform)?)),
                (Route::Simd, Some(simd)),
                (Route::Scalar, $crate::backend::route::mechanism::routes!(@optional $($scalar)?)),
            ];
            // Invalid cells fail const evaluation, before audio processing.
            const SELECTED: Kernel = ROUTE.select(FORMS).expect("selected kernel form is unavailable on this target");

            #[inline(always)]
            pub(crate) fn invoke($($argument: $ty),*) -> $result {
                SELECTED($($argument),*)
            }

            #[inline(always)]
            fn simd($($argument: $ty),*) -> $result {
                fearless_simd::dispatch!(fearless_simd::Level::new(), simd => $simd(simd, $($argument),*))
            }

            #[cfg(any(test, feature = "routes"))]
            pub(crate) fn routes() -> impl Iterator<Item = (&'static str, Kernel)> {
                FORMS.iter().filter_map(|&(route, form)| form.map(|kernel| (route.name(), kernel)))
                    .chain([("entry", $entry as Kernel)])
            }
        }
    )+};
    (@optional $form:path) => { Some($form) };
    (@optional) => { None };
    (@platform $form:path) => {{
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        { Some($form) }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        { None }
    }};
    (@platform) => { None };
}

pub(crate) use route;
pub(super) use routes;
