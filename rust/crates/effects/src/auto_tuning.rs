//! Every target number and cap of Auto colour grading, in one place for tuning (brightness, colour and percentiles are on the grade's 0..1 scale unless a name says otherwise).

// ── Looking at the picture ───────────────────────────────────────────

/// Seconds of steady frames the measurement covers.
pub const MEASURE_SECONDS: f64 = 1.0;
/// Milliseconds the button keeps saying how a press went.
pub const LABEL_MS: u64 = 3000;
/// Milliseconds Auto waits for the vision service's first answer (it may be loading its models) before going by the whole picture.
pub const HOLD_TIMEOUT_MS: u64 = 6000;
/// Milliseconds after which a measurement gives up and works with the frames it has.
pub const MEASURE_TIMEOUT_MS: u64 = 4000;
/// Milliseconds between two frames that get looked at.
pub const FRAME_EVERY_MS: u64 = 80;
/// Fewest steady frames a measurement is worth anything with.
pub const MIN_FRAMES: usize = 6;
/// Width in pixels of the shrunk copy every measurement is made on.
pub const SMALL_WIDTH: usize = 160;
/// Landmarks older or newer than the frame by more than this describe some other moment (nanoseconds).
pub const FRESH_NS: u64 = 500_000_000;
/// A frame whose picture changed more than this (mean brightness) since the last one looked at is skipped as movement.
pub const MOTION_SKIP: f32 = 0.03;

// ── Finding the face ─────────────────────────────────────────────────

/// Eyes closer than this (pixels on the shrunk copy) make a face too small to read colour from.
pub const MIN_EYE_PX: f64 = 6.0;
/// The nose sits between these many eye distances below the eyes, or the landmarks are nonsense.
pub const NOSE_DROP: (f64, f64) = (0.25, 1.2);
/// How far below the eyes (in eye distances) the skin ellipse's centre is.
pub const FACE_CENTRE_DOWN: f64 = 0.35;
/// The skin ellipse's half width in eye distances.
pub const FACE_ACROSS: f64 = 0.85;
/// The skin ellipse's half height in eye distances.
pub const FACE_ALONG: f64 = 1.2;
/// With both ears seen the ellipse is no wider than this share of the ear distance (a turned head).
pub const FACE_EAR_SHARE: f64 = 0.38;
/// A face further than this (of the picture) from the one first followed is someone else.
pub const SAME_PERSON: f64 = 0.25;
/// Less of the ellipse than this covered by the person mask means the mask is old or the face moved off.
pub const MASK_FACE_COVER: f32 = 0.6;
/// Person mask level (0..255) from which a pixel counts as the person.
pub const MASK_PERSON: u8 = 128;
/// Person mask level (0..255) below which a pixel can be background.
pub const MASK_BACKGROUND: u8 = 32;
/// A person's box from their landmarks grows by this share of its size to keep the background clear of them.
pub const PERSON_GROW: f64 = 0.35;

// ── Picking skin ─────────────────────────────────────────────────────

/// Face pixels darker than this (eyes, hair, shadow) are no use as skin.
pub const SKIN_LUMA_MIN: f32 = 0.05;
/// A channel at or above this means the camera clipped the pixel.
pub const CLIPPED: f32 = 0.985;
/// Skin pixels this many spreads (median absolute deviation) off the middle colour are left out.
pub const SKIN_OUTLIER_CHROMA: f32 = 2.5;
/// Skin pixels this many spreads off the middle brightness are left out.
pub const SKIN_OUTLIER_LUMA: f32 = 3.0;
/// Fewer skin pixels than this make a frame's face uncertain, and it's thrown out.
pub const MIN_SKIN_PIXELS: usize = 60;
/// Less of the face than this kept as skin makes a frame's face uncertain.
pub const MIN_SKIN_KEPT: f32 = 0.35;
/// A face whose colour spreads more than this (log red over green, log blue over green) is makeup, a beard or glasses glare.
pub const SKIN_SPREAD_MAX: f32 = 0.10;
/// The floor under every spread, so a flat picture doesn't throw out noise.
pub const SPREAD_FLOOR: f32 = 0.01;
/// Frames with a face needed before the face counts.
pub const MIN_FACE_FRAMES: usize = 3;
/// And the share of the steady frames they have to be.
pub const MIN_FACE_SHARE: f32 = 0.4;
/// The face's colour may vary this much between frames (log ratio) before it's called unsteady.
pub const FACE_STEADY: f32 = 0.05;

// ── Near-grey background ─────────────────────────────────────────────

/// The shrunk copy is cut into this many columns and rows of cells that can each be a grey patch.
pub const GRID: (usize, usize) = (8, 5);
/// A cell needs this share of it to be background to be a patch.
pub const PATCH_BACKGROUND: f32 = 0.7;
/// A patch's mean brightness lies between these.
pub const PATCH_LUMA: (f32, f32) = (0.25, 0.90);
/// A patch has at most this share of clipped pixels.
pub const PATCH_CLIPPED: f32 = 0.02;
/// A patch's brightness varies by less than this (standard deviation), so it's a smooth wall.
pub const PATCH_SMOOTH: f32 = 0.05;
/// A patch's cast (log ratios in linear light) is at most this far from grey.
pub const GREY_NEAR: f32 = 0.30;
/// A cell has to be a patch in this share of the frames to count.
pub const PATCH_STEADY: f32 = 0.5;
/// Separate patches needed for the evidence to count.
pub const MIN_PATCHES: usize = 3;
/// Patches count as separate when their cells are at least this many cells apart.
pub const PATCH_APART: usize = 2;
/// Patches whose casts differ by less than this agree.
pub const GREY_AGREE: f32 = 0.06;
/// Patches whose casts differ by more than this conflict, and in between the evidence is unsure.
pub const GREY_CONFLICT: f32 = 0.14;

// ── Tone ─────────────────────────────────────────────────────────────

/// A pixel with every channel at or below this is black clipped.
pub const BLACK_CLIPPED: f32 = 0.012;
/// A share of the pixels clipped at an end of the range this big means the picture already reaches that end.
pub const CLIPPED_ENDS: f32 = 0.01;
/// Brightness from which a pixel counts as close to clipping.
pub const NEAR_CLIP_LUMA: f32 = 0.92;
/// Brightness range the colour strength is averaged over.
pub const CHROMA_LUMA: (f32, f32) = (0.10, 0.90);
/// Sample pixels the fit works on: skin, background, the whole picture.
pub const SAMPLES: (usize, usize, usize) = (500, 700, 1500);

// ── White balance ────────────────────────────────────────────────────

/// Skin sits within this many degrees of the skin line on a vectorscope, and a face inside is left alone.
pub const SKIN_BAND_DEG: f32 = 10.0;
/// Once skin is off the skin line by more than the band, the correction aims for within this many degrees of it.
pub const SKIN_AIM_DEG: f32 = 3.0;
/// How much of the way a correction goes when there's a face to go by.
pub const WB_STRENGTH: f64 = 0.9;
/// How much of the way a correction goes with only grey patches to go by.
pub const WB_GREY_ONLY_STRENGTH: f64 = 0.8;
/// The most Temperature Auto sets.
pub const WB_MAX_TEMPERATURE: f64 = 100.0;
/// The most Tint Auto sets.
pub const WB_MAX_TINT: f64 = 100.0;
/// Face and grey patches wanting Temperature in opposite directions this far apart conflict.
pub const WB_CONFLICT_TEMPERATURE: f64 = 30.0;
/// Face and grey patches wanting Tint in opposite directions this far apart conflict.
pub const WB_CONFLICT_TINT: f64 = 30.0;
/// A white balance smaller than this on both sliders isn't worth setting.
pub const WB_MIN: f64 = 2.0;
/// Weight of the face's distance outside the skin band, per band width squared.
pub const WB_FACE_WEIGHT: f64 = 1.0;
/// Weight of the grey patches' cast, per squared agreement width.
pub const WB_GREY_WEIGHT: f64 = 1.0;
/// Cost of moving Temperature, per ten units squared.
pub const WB_COST_TEMPERATURE: f64 = 0.01;
/// Cost of moving Tint, per ten units squared, lower since a hue off the skin line is mostly tint.
pub const WB_COST_TINT: f64 = 0.02;

/// With nothing to go by but the whole picture, a colour cast (log ratios in linear light) must be bigger than this to be corrected.
pub const WORLD_MIN: f32 = 0.35;
/// And smaller than this, or it is more likely coloured light than a lamp's cast.
pub const WORLD_MAX_CAST: f32 = 0.60;
/// How much of the way a correction goes when it only goes by the whole picture being grey on average.
/// Plus this much more per unit of cast past that, up to the strength a face gets.
/// The most Temperature and Tint a correction going by the whole picture alone may set: warm rooms are often meant to be warm.
pub const WB_WORLD_MAX: (f64, f64) = (50.0, 35.0);
/// The same for a picture with a face in it, which vouches that the light is what's off.
pub const WORLD_MIN_FACE: f32 = 0.22;
pub const WB_WORLD_STRENGTH_FACE: f64 = 0.3;
pub const WB_WORLD_RAMP_FACE: f64 = 4.0;
pub const WB_WORLD_RAMP: f64 = 1.0;
pub const WB_WORLD_STRENGTH: f64 = 0.25;

// ── Brightness ───────────────────────────────────────────────────────

/// A face darker than this has lost its detail.
pub const FACE_DARK_BELOW: f32 = 0.20;
/// And is brought up to this.
pub const FACE_DARK_TARGET: f32 = 0.30;
/// A face brighter than this is nearly gone.
pub const FACE_BRIGHT_ABOVE: f32 = 0.80;
/// And is brought down to this.
pub const FACE_BRIGHT_TARGET: f32 = 0.70;
/// A dim face only counts as lost detail when the picture's middle brightness is below this too, else it's dark skin or a backlit person.
pub const PICTURE_DARK_BELOW: f32 = 0.28;
/// A face with more than this share clipped is pulled down too.
pub const FACE_CLIPPED_ABOVE: f32 = 0.04;
/// With no face, a picture whose middle brightness is below this is too dark.
pub const SCENE_DARK_BELOW: f32 = 0.10;
/// And is brought up towards this.
pub const SCENE_DARK_TARGET: f32 = 0.26;
/// A middle brightness below this is a covered lens or a dark room's noise, and is left alone.
pub const SCENE_BLACK_BELOW: f32 = 0.012;
/// With no face, a middle brightness above this, or this share of the picture clipped to white, is overexposed.
pub const SCENE_BRIGHT_ABOVE: f32 = 0.78;
pub const SCENE_BLOWN_ABOVE: f32 = 0.25;
/// And is brought down towards this (clipped highlights can't come back, so the midtones are what's aimed at).
pub const SCENE_BRIGHT_TARGET: f32 = 0.62;
/// Or this when over a third of the picture is blown out, so that more of it comes back below white.
pub const SCENE_BLOWN_TARGET: f32 = 0.58;
/// Lowest and highest Gamma master Auto sets.
pub const GAMMA_RANGE: (f64, f64) = (-0.6, 0.7);
/// Lowest and highest Gain master Auto sets.
pub const GAIN_RANGE: (f64, f64) = (1.0, 1.80);
/// Share of the unclipped pixels a grade may newly clip before it costs anything.
pub const NEW_CLIP_OK: f32 = 0.005;

// ── Levels ───────────────────────────────────────────────────────────

/// Levels are only corrected when the picture spans at least this much brightness, since a flat wall says nothing about its blacks.
pub const LEVELS_MIN_SPREAD: f32 = 0.20;
/// An unclipped darkest 1% above this means milky blacks.
pub const BLACK_HIGH: f32 = 0.16;
/// An unclipped darkest 1% below this can be lifted when the picture is quiet enough.
pub const BLACK_LOW: f32 = 0.015;
/// Where the darkest 1% should sit.
pub const BLACK_TARGET: f32 = 0.07;
/// Most share of black clipped pixels that still leaves it worth lifting.
pub const BLACK_CLIP_MAX: f32 = 0.04;
/// Noise level from which Lift stops adding, since lifting shadows lifts the noise with them.
pub const NOISE_LIFT_MAX: f32 = 0.02;
/// Lowest and highest Lift master Auto sets.
pub const LIFT_RANGE: (f64, f64) = (-0.10, 0.04);
/// A spread (brightest 1% minus darkest 1%) below this is flat.
pub const SPREAD_FLAT: f32 = 0.60;
/// And goes towards this.
pub const SPREAD_TARGET: f32 = 0.72;
/// Lowest and highest Contrast Auto sets, so a punchy picture never loses any.
pub const CONTRAST_RANGE: (f64, f64) = (1.0, 1.35);
/// Range the pivot may go in when it moves near the face.
pub const PIVOT_RANGE: (f64, f64) = (0.25, 0.65);
/// How far lift and contrast may move the face's brightness before they're charged for it.
pub const FACE_STAY: f32 = 0.05;

/// A picture whose brightest 1% sits below this has nothing near white in it, so it is underexposed.
pub const TOP_DARK_BELOW: f32 = 0.72;
/// Unless its darkest 1% is above this, which is haze or milky blacks rather than too little light.
pub const TOP_BLACK_BELOW: f32 = 0.12;
/// And its brightest 1% is brought up towards this.
pub const TOP_TARGET: f32 = 0.85;

// ── Highlights and saturation ────────────────────────────────────────

/// More than this share of the picture close to clipping gets pulled down.
pub const NEAR_CLIP_ABOVE: f32 = 0.06;
/// To about this share.
pub const NEAR_CLIP_TARGET: f32 = 0.03;
/// Lowest and highest Highlights Auto sets.
pub const HIGHLIGHTS_RANGE: (f64, f64) = (-40.0, 0.0);
/// A mean colour strength (largest channel minus smallest) below this is dull.
pub const CHROMA_DULL_BELOW: f32 = 0.05;
/// And goes towards this.
pub const CHROMA_DULL_TARGET: f32 = 0.09;
/// A mean colour strength above this is loud.
pub const CHROMA_LOUD_ABOVE: f32 = 0.30;
/// And goes towards this.
pub const CHROMA_LOUD_TARGET: f32 = 0.24;
/// Lowest and highest Saturation Auto sets.
pub const SATURATION_RANGE: (f64, f64) = (0.80, 1.30);
/// Skin saturation (HSV) may not pass this.
pub const SKIN_SAT_MAX: f32 = 0.60;
/// Nor grow past this times what it was.
pub const SKIN_SAT_GROWTH: f32 = 1.15;
/// Plus this.
pub const SKIN_SAT_SLACK: f32 = 0.03;
/// Skin hue may not turn this many degrees further from the skin line than it was, which is towards orange.
pub const SKIN_HUE_SLACK_DEG: f32 = 3.0;

// ── The fit ──────────────────────────────────────────────────────────

/// A stage's best candidate has to cost less than this share of changing nothing to be used.
pub const IMPROVE_MARGIN: f64 = 0.85;
/// Most evaluations of the grade one stage makes.
pub const STAGE_BUDGET: usize = 400;
/// Cost per unit squared of each slider's move from neutral: lift, gamma, gain, contrast, pivot, highlights, saturation.
pub const MOVE_COST: [f64; 7] = [20.0, 4.0, 12.0, 6.0, 1.0, 0.002, 6.0];
/// The person's own wheels and curves on top may clip this much more of the face than they would alone.
pub const FINAL_FACE_CLIP: f32 = 0.06;
