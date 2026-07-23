use joey_tui::anim::{Activity, Clock, Equalizer, ParticleField, Pulse, Spinner};
use joey_tui::theme::Theme;
use std::time::Duration;

fn main() {
    let theme = Theme::aurora();
    let mut activity = Activity::idle();
    let mut field = ParticleField::new(120, 40);
    let mut spinner = Spinner::dots();
    let mut orbit = Spinner::orbit();
    let mut eq = Equalizer::new(28);
    let mut pulse = Pulse::new();
    let dt = Duration::from_millis(33);

    // Simulate ~500,000 frames (~4.5 hours at 30fps) alternating busy/idle,
    // as would happen over many long turns in a session.
    for i in 0..500_000u64 {
        let target = if (i / 300) % 2 == 0 { 4 } else { 0 };
        activity.update(target, dt);
        let speed = activity.speed();
        spinner.tick(dt, speed);
        orbit.tick(dt, speed);
        field.tick(dt, activity, theme);
        eq.tick(dt, activity);
        pulse.tick(dt, activity);
        if i % 50_000 == 0 {
            println!("frame {i}: particles={}", field.particles().len());
        }
    }
    println!("final particles={}", field.particles().len());
}
