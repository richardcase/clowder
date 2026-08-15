Resizing a window (or a pane) now reflows the terminal to fill the new size correctly, instead of
leaving dead space when you shrink it or a stale edge when you grow it. Each pane's size is also
remembered, so if an agent's daemon restarts it comes back at the size you left it rather than the
default 80x24.
