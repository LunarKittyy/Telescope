use crate::imgops::Gray;

#[test]
fn firming_pulls_the_ends_to_solid_and_keeps_the_middle_smooth() {
    let ramp = Gray { w: 256, h: 1, data: (0..=255).collect() };
    let firm = ramp.firm(40, 170);
    assert!(firm.data[..=40].iter().all(|&v| v == 0));
    assert!(firm.data[170..].iter().all(|&v| v == 255));
    assert!(firm.data.windows(2).all(|w| w[0] <= w[1]));
    // half way between the ends is half way
    assert_eq!(firm.data[105], 128);
    assert_eq!(ramp.firm(0, 255), ramp);
}
