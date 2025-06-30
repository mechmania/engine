use serde::{ Serialize, Deserialize };
use std::ops::{ Add, AddAssign, Sub, SubAssign, Mul, MulAssign, Div, DivAssign, Neg };

pub use std::f32::consts::PI;

#[derive(Serialize, Deserialize, Clone, PartialEq, Copy, Debug)]
#[repr(C)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}


impl Vec2 {
    pub const ZERO: Self = Vec2 { x: 0.0, y: 0.0 };

    #[inline(always)]
    pub fn from_angle_rad(angle_rad: f32) -> Self {
        let (sin, cos) = angle_rad.sin_cos();
        Vec2 { x: cos, y: sin }
    }

    #[inline(always)]
    pub fn from_angle_deg(angle_deg: f32) -> Self {
        Self::from_angle_rad(angle_deg.to_radians())
    }

    #[inline(always)]
    pub fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }

    #[inline(always)]
    pub fn dot(self, other: Vec2) -> f32 {
        self.x * other.x + self.y * other.y
    }

    #[inline(always)]
    pub fn norm_sq(&self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    #[inline(always)]
    pub fn norm(&self) -> f32 {
        self.norm_sq().sqrt()
    }

    #[inline(always)]
    pub fn angle_rad(&self) -> f32 {
        self.y.atan2(self.x)
    }

    #[inline(always)]
    pub fn angle_deg(&self) -> f32 {
        self.angle_rad().to_degrees()
    }

    pub fn normalize_or_zero(mut self) -> Self {
        let norm = self.norm();
        if norm == 0.0 {
            return Vec2::ZERO;
        }
        self.x /= norm;
        self.y /= norm;
        self
    }
    
    pub fn normalize_or_else(mut self, fallback: impl FnOnce() -> Self) -> Self {
        let norm = self.norm();
        if norm == 0.0 {
            return fallback();
        }
        self.x /= norm;
        self.y /= norm;
        self
    }

    pub fn rotate_rad(mut self, angle_rad: f32) -> Self {
        let (sin, cos) = angle_rad.sin_cos();
        (self.x, self.y) = (
            self.x * cos - self.y * sin,
            self.x * sin + self.y * cos
        );
        self
    }
    
    #[inline(always)]
    pub fn rotate_deg(self, angle_deg: f32) -> Self {
        self.rotate_rad(angle_deg.to_radians())
    }

    #[inline(always)]
    pub fn dist_sq(&self, other: &Vec2) -> f32 {
        (*other - *self).norm_sq()
    }

    #[inline(always)]
    pub fn dist(&self, other: &Vec2) -> f32 {
        (*other - *self).norm()
    }
}

impl Add<Vec2> for Vec2 {
    type Output = Vec2;
    fn add(mut self: Vec2, other: Vec2) -> Vec2 {
        self.x += other.x;
        self.y += other.y;
        self
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, other: Vec2) {
        self.x += other.x;
        self.y += other.y
    }
}

impl Sub for Vec2 {
    type Output = Vec2;

    fn sub(mut self, other: Vec2) -> Vec2 {
        self.x -= other.x;
        self.y -= other.y;
        self
    }
}

impl SubAssign for Vec2 {
    fn sub_assign(&mut self, other: Vec2) {
        self.x -= other.x;
        self.y -= other.y;
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;

    fn mul(mut self, scalar: f32) -> Vec2 {
        self.x *= scalar;
        self.y *= scalar;
        self
    }
}

impl Mul<Vec2> for f32 {
    type Output = Vec2;

    fn mul(self, mut vec: Vec2) -> Vec2 {
        vec.x *= self;
        vec.y *= self;
        vec
    }
}

impl MulAssign<f32> for Vec2 {
    fn mul_assign(&mut self, scalar: f32) {
        self.x *= scalar;
        self.y *= scalar;
    }
}

impl Div<f32> for Vec2 {
    type Output = Vec2;

    fn div(mut self, scalar: f32) -> Vec2 {
        self.x /= scalar;
        self.y /= scalar;
        self
    }
}

impl DivAssign<f32> for Vec2 {
    fn div_assign(&mut self, scalar: f32) {
        self.x /= scalar;
        self.y /= scalar;
    }
}

impl Neg for Vec2 {
    type Output = Vec2;

    fn neg(self) -> Vec2 {
        Vec2 {
            x: -self.x,
            y: -self.y,
        }
    }
}

impl Default for Vec2 {
    fn default() -> Self {
        Vec2::ZERO
    }
}
