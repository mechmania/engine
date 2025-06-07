import json
import sys
import pygame
from pathlib import Path

# ------------------ Configurable colours ------------------
BG_COLOR = (0, 0, 0)
UI_BG_COLOR = (40, 40, 40)
PADDLE_COLOR = (255, 255, 255)
BALL_COLOR = (255, 255, 255)
SLIDER_FILL = (0, 120, 200)
PLAY_CLR = (200, 100, 100)
PAUSE_CLR = (100, 200, 100)
TXT_COLOR = (255, 255, 255)


class PongReplay:
    """Visualises a logged Pong match using pygame.

    Log coordinate system:
      * (0, 0) is the court centre
      * +x → right, −x → left
      * +y → up,   −y → down
      * p0_pos / p1_pos give the **paddle centre** y‑coordinate in that system

    The visualiser converts these Cartesian coordinates to pygame screen
    space (origin top‑left, +y down).
    """

    UI_HEIGHT = 60
    MAX_FPS = 60
    PLAY_RECT = pygame.Rect(10, 0, 40, 40)  # y set after court height known

    # ------------------------------------------------------------------
    # Construction / data loading
    # ------------------------------------------------------------------
    def __init__(self, logfile: Path):
        self.cfg, self.states = self._load_log(logfile)
        if not self.states:
            raise ValueError("Log contains no game states")

        # Pre‑compute centre of the court in screen coords
        self.cx = self.cfg["width"] // 2
        self.cy = self.cfg["height"] // 2

        # Playback state
        self.idx = 0
        self.playing = False
        self.speed = 1  # ticks/frame

        # Pygame init
        pygame.init()
        h = self.cfg["height"] + self.UI_HEIGHT
        self.screen = pygame.display.set_mode((self.cfg["width"], h))
        pygame.display.set_caption("Pong Visualiser")
        self.clock = pygame.time.Clock()
        self.font = pygame.font.SysFont(None, 24)

        # UI rects depending on dimensions
        self.PLAY_RECT.y = self.cfg["height"] + 10
        self.slider = pygame.Rect(70, self.cfg["height"] + 20, self.cfg["width"] - 80, 20)

    @staticmethod
    def _load_log(path: Path):
        with open(path, "r", encoding="utf-8") as f:
            lines = f.read().splitlines()
        cfg = json.loads(lines[0])
        states = [json.loads(l) for l in lines[1:]]
        return cfg, states

    # ------------------------------------------------------------------
    # Coordinate helpers
    # ------------------------------------------------------------------
    def _to_screen(self, x: float, y: float):
        """Convert Cartesian (centre‑based) -> pygame screen coordinate tuple."""
        return int(self.cx + x), int(self.cy - y)

    def _paddle_rect(self, x_left: int, centre_y: float):
        """Return pygame.Rect for a paddle given its LEFT x and centre‑y (log units)."""
        # Convert centre‑y to top‑left screen coordinate
        top_screen_y = self.cy - centre_y - self.cfg["paddle_length"] // 2
        return pygame.Rect(x_left, int(top_screen_y), self.cfg["paddle_width"], self.cfg["paddle_length"])

    # ------------------------------------------------------------------
    # Main loop
    # ------------------------------------------------------------------
    def run(self):
        while True:
            self._handle_events()
            if self.playing:
                self.idx += self.speed
                if self.idx >= len(self.states):
                    self.idx = len(self.states) - 1
                    self.playing = False
            self._draw()
            pygame.display.flip()
            self.clock.tick(self.MAX_FPS)

    # ------------------------------------------------------------------
    # Input handling
    # ------------------------------------------------------------------
    def _handle_events(self):
        for ev in pygame.event.get():
            if ev.type == pygame.QUIT:
                pygame.quit()
                sys.exit()
            if ev.type == pygame.KEYDOWN:
                if ev.key == pygame.K_SPACE:
                    self.playing = not self.playing
                elif ev.key in (pygame.K_RIGHT, pygame.K_PERIOD, pygame.K_EQUALS):
                    self.speed = min(32, self.speed * 2)
                elif ev.key in (pygame.K_LEFT, pygame.K_COMMA, pygame.K_MINUS):
                    self.speed = max(1, self.speed // 2)
            if ev.type == pygame.MOUSEBUTTONDOWN and ev.button == 1:
                if self.PLAY_RECT.collidepoint(ev.pos):
                    self.playing = not self.playing
                elif self.slider.collidepoint(ev.pos):
                    self._scrub(ev.pos)
            if ev.type == pygame.MOUSEMOTION and ev.buttons[0] and self.slider.collidepoint(ev.pos):
                self._scrub(ev.pos)

    def _scrub(self, pos):
        rel = (pos[0] - self.slider.x) / self.slider.w
        rel = max(0.0, min(1.0, rel))
        self.idx = int(rel * (len(self.states) - 1))
        self.playing = False

    # ------------------------------------------------------------------
    # Rendering
    # ------------------------------------------------------------------
    def _draw(self):
        self.screen.fill(BG_COLOR)
        state = self.states[self.idx]

        # Paddles (centre‑y stored in log)
        p0_rect = self._paddle_rect(0, state["p0_pos"])
        p1_rect = self._paddle_rect(self.cfg["width"] - self.cfg["paddle_width"], state["p1_pos"])
        pygame.draw.rect(self.screen, PADDLE_COLOR, p0_rect)
        pygame.draw.rect(self.screen, PADDLE_COLOR, p1_rect)

        # Ball (centre position in log)
        bx, by = self._to_screen(*state["ball_pos"])
        pygame.draw.circle(self.screen, BALL_COLOR, (bx, by), self.cfg["ball_radius"])

        # UI background bar
        pygame.draw.rect(self.screen, UI_BG_COLOR, (0, self.cfg["height"], self.cfg["width"], self.UI_HEIGHT))

        # Play / Pause button
        btn_clr = PAUSE_CLR if self.playing else PLAY_CLR
        pygame.draw.rect(self.screen, btn_clr, self.PLAY_RECT)
        if self.playing:
            # pause symbol
            bar_w = 8
            pad = 8
            y = self.PLAY_RECT.y + 8
            pygame.draw.rect(self.screen, BG_COLOR, (self.PLAY_RECT.x + pad, y, bar_w, 24))
            pygame.draw.rect(self.screen, BG_COLOR, (self.PLAY_RECT.x + pad + bar_w + 4, y, bar_w, 24))
        else:
            # play triangle
            pts = [
                (self.PLAY_RECT.x + 10, self.PLAY_RECT.y + 8),
                (self.PLAY_RECT.x + 10, self.PLAY_RECT.y + 32),
                (self.PLAY_RECT.x + 30, self.PLAY_RECT.y + 20),
            ]
            pygame.draw.polygon(self.screen, BG_COLOR, pts)

        # Speed indicator
        sp_txt = self.font.render(f"x{self.speed}", True, TXT_COLOR)
        self.screen.blit(sp_txt, (self.PLAY_RECT.right + 10, self.PLAY_RECT.y + 10))

        # Slider background & fill
        pygame.draw.rect(self.screen, TXT_COLOR, self.slider, 2)
        filled = int(self.slider.w * self.idx / (len(self.states) - 1))
        pygame.draw.rect(self.screen, SLIDER_FILL, (self.slider.x, self.slider.y, filled, self.slider.h))

        # Tick counter
        tk_txt = self.font.render(f"{state['tick']}/{len(self.states) - 1}", True, TXT_COLOR)
        self.screen.blit(tk_txt, (self.slider.right - 70, self.slider.y - 2))


# -------------------------------------------------------------------------
# Entry point
# -------------------------------------------------------------------------
if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python pong_visualizer.py path/to/log.txt")
        sys.exit(1)

    try:
        PongReplay(Path(sys.argv[1])).run()
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)

