use kithara_test_utils::kithara;
use kithara_ui_capture::Film;

fn film(photos: usize, steps: usize) -> Film<&'static str> {
    Film::new(vec!["motion", "lottie"], photos, steps).expect("a well formed film")
}

#[kithara::test]
fn a_still_set_takes_one_photograph_of_each_page() {
    assert_eq!(Film::stills(vec!["motion"]).photos, 1);
}

#[kithara::test]
fn a_page_photographed_once_keeps_its_own_name() {
    assert_eq!(film(1, 0).file(&"motion", 0), "motion.png");
}

#[kithara::test]
fn a_page_photographed_more_than_once_numbers_its_photographs() {
    assert_eq!(film(2, 1).file(&"motion", 1), "motion-001.png");
}

#[kithara::test]
fn a_film_of_no_photographs_is_refused() {
    assert!(Film::new(vec!["motion"], 0, 3).is_err());
}

#[kithara::test]
fn a_film_of_several_photographs_with_no_time_between_them_is_refused() {
    assert!(Film::new(vec!["motion"], 2, 0).is_err());
}
