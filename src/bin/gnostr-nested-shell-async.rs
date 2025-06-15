use std::{
    io::{self, BufWriter, Read, Write},
    sync::{Arc, RwLock},
    time::Duration,
};

use bytes::Bytes;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    style::ResetColor,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::Alignment,
    style::{Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use tokio::{
    sync::mpsc::{channel, Sender},
    task,
};
use tui_term::widget::PseudoTerminal;
use vt100::Screen;

#[derive(Debug)]
struct Size {
    cols: u16,
    rows: u16,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, ResetColor)?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    //BEGIN: gnostr child process
    let pty_system = NativePtySystem::default();
    let cwd = std::env::current_dir().unwrap();
    //let mut cmd = CommandBuilder::new_default_prog();
    let mut cmd = CommandBuilder::new("gnostr");
    cmd.cwd(cwd);

    let size = Size {
        rows: terminal.size()?.height,
        cols: terminal.size()?.width,
    };

    let pair = pty_system
        .openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    // Wait for the child to complete
    task::spawn_blocking(move || {
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let _child_exit_status = child.wait().unwrap();
        drop(pair.slave);
    });

    let mut reader = pair.master.try_clone_reader().unwrap();
    let parser = Arc::new(RwLock::new(vt100::Parser::new(size.rows - 5, size.cols, 0)));

    {
        let parser = parser.clone();
        task::spawn_blocking(move || {
            // Consume the output from the child
            // Can't read the full buffer, since that would wait for EOF
            let mut buf = [0u8; 8192];
            let mut processed_buf = Vec::new();
            loop {
                let size = reader.read(&mut buf).unwrap();
                if size == 0 {
                    break;
                }
                if size > 0 {
                    processed_buf.extend_from_slice(&buf[..size]);
                    let mut parser = parser.write().unwrap();
                    parser.process(&processed_buf);

                    // Clear the processed portion of the buffer
                    processed_buf.clear();
                }
            }
        });
    }

    let (tx, mut rx) = channel::<Bytes>(32);

    let mut writer = BufWriter::new(pair.master.take_writer().unwrap());

    // Drop writer on purpose
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            writer.write_all(&bytes).unwrap();
            writer.flush().unwrap();
        }
        drop(pair.master);
    });
    //END: gnostr child process

    run(&mut terminal, parser, tx).await?;

    // restore terminal
    disable_raw_mode()?;
    //twice for child process
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    terminal.show_cursor()?;
    terminal.show_cursor()?;
    println!("{size:?}");
    std::process::exit(0);
    #[allow(unreachable_code)]
    Ok(())
}

async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    parser: Arc<RwLock<vt100::Parser>>,
    sender: Sender<Bytes>,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, parser.read().unwrap().screen()))?;

        // Event read is non-blocking with a 10ms timeout
        if event::poll(Duration::from_millis(10))? {
            // It's guaranteed that `read()` won't block when `poll()` returns `true`
            match event::read()? {
                Event::Key(key) => {
                    // We only handle key press events, not releases
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            // --- Quit Keys ---
                            KeyCode::Char('\\') => return Ok(()),
                            //WE DISABLE q: for gnostr editor mode such as in vim :q
                            //KeyCode::Char('q') => return Ok(()),
                            KeyCode::Esc => {
                                sender.send(Bytes::from(vec![27])).await.unwrap();
                            }/*return Ok(())*/, // Also a common quit key
                            KeyCode::Char(input) => {
                                let bytes_to_send = if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    match input {
                                        // Special handling for Ctrl+C
                                        'c' | 'C' => Bytes::from(vec![3]), // ASCII ETX
                                        // You can add more Ctrl+char combinations here if needed.
                                        // For example, Ctrl+D is 4 (EOT), Ctrl+Z is 26 (SUB).
                                        // 'd' | 'D' => Bytes::from(vec![4]),
                                        // 'z' | 'Z' => Bytes::from(vec![26]),
                                        _ => {
                                            // Fallback for other Ctrl+char combinations:
                                            // Convert to uppercase, then subtract 64 (ASCII for '@')
                                            let ascii_val = input.to_ascii_uppercase() as u8;
                                            if (64..=95).contains(&ascii_val) { // Covers A-Z, [, \, ], ^, _
                                                Bytes::from(vec![ascii_val - 64])
                                            } else {
                                                // If it's a Ctrl+char not in the A-Z range or a special case,
                                                // you might still want to send the raw char bytes or ignore.
                                                // Sending raw char bytes means the remote might not interpret it as Ctrl.
                                                // For robustness, consider if you truly need to send every Ctrl+char.
                                                Bytes::from(input.to_string().into_bytes())
                                            }
                                        }
                                    }
                                } else {
                                    // No Ctrl modifier, send the character directly
                                    Bytes::from(input.to_string().into_bytes())
                                };
                                sender.send(bytes_to_send).await.unwrap();
                            }

                            KeyCode::Backspace => {
                                sender.send(Bytes::from(vec![8])).await.unwrap();
                            }
                            KeyCode::Enter => sender.send(Bytes::from(vec![10])).await.unwrap(),
                            KeyCode::Tab => sender.send(Bytes::from(vec![9])).await.unwrap(),
                            // BackTab is Shift+Tab, often sent as ESC[Z
                            KeyCode::BackTab => {
                                sender.send(Bytes::from(vec![27, 91, 90])).await.unwrap()
                            }

                            // --- Arrow Keys ---
                            KeyCode::Left => {
                                sender.send(Bytes::from(vec![27, 91, 68])).await.unwrap()
                            }
                            KeyCode::Right => {
                                sender.send(Bytes::from(vec![27, 91, 67])).await.unwrap()
                            }
                            KeyCode::Up => {
                                sender.send(Bytes::from(vec![27, 91, 65])).await.unwrap()
                            }
                            KeyCode::Down => {
                                sender.send(Bytes::from(vec![27, 91, 66])).await.unwrap()
                            }

                            // --- Navigation and Edit Keys (using common xterm/VT sequences) ---
                            KeyCode::Home => {
                                sender.send(Bytes::from(vec![27, 91, 72])).await.unwrap()
                                // ESC[H
                            }
                            KeyCode::End => {
                                sender.send(Bytes::from(vec![27, 91, 70])).await.unwrap()
                                // ESC[F
                            }
                            KeyCode::PageUp => {
                                sender
                                    .send(Bytes::from(vec![27, 91, 53, 126]))
                                    .await
                                    .unwrap() // ESC[5~
                            }
                            KeyCode::PageDown => {
                                sender
                                    .send(Bytes::from(vec![27, 91, 54, 126]))
                                    .await
                                    .unwrap() // ESC[6~
                            }
                            KeyCode::Delete => {
                                sender
                                    .send(Bytes::from(vec![27, 91, 51, 126]))
                                    .await
                                    .unwrap() // ESC[3~
                            }
                            KeyCode::Insert => {
                                sender
                                    .send(Bytes::from(vec![27, 91, 50, 126]))
                                    .await
                                    .unwrap() // ESC[2~
                            }

                            // --- Function Keys ---
                            KeyCode::F(n) => {
                                let seq = match n {
                                    1 => vec![27, 79, 80],           // \x1bOP
                                    2 => vec![27, 79, 81],           // \x1bOQ
                                    3 => vec![27, 79, 82],           // \x1bOR
                                    4 => vec![27, 79, 83],           // \x1bOS
                                    5 => vec![27, 91, 49, 53, 126],  // \x1b[15~
                                    6 => vec![27, 91, 49, 55, 126],  // \x1b[17~
                                    7 => vec![27, 91, 49, 56, 126],  // \x1b[18~
                                    8 => vec![27, 91, 49, 57, 126],  // \x1b[19~
                                    9 => vec![27, 91, 50, 48, 126],  // \x1b[20~
                                    10 => vec![27, 91, 50, 49, 126], // \x1b[21~
                                    11 => vec![27, 91, 50, 51, 126], // \x1b[23~
                                    12 => vec![27, 91, 50, 52, 126], // \x1b[24~
                                    _ => vec![],                     // F13+ are less common
                                };
                                if !seq.is_empty() {
                                    sender.send(Bytes::from(seq)).await.unwrap();
                                }
                            }

                            // --- Other Keys ---
                            KeyCode::Null => {
                                sender.send(Bytes::from(vec![0])).await.unwrap();
                            }

                            // --- Keys that typically don't send sequences are ignored ---
                            KeyCode::CapsLock => {}
                            KeyCode::ScrollLock => {}
                            KeyCode::NumLock => {}
                            KeyCode::PrintScreen => {}
                            KeyCode::Pause => {}
                            KeyCode::Menu => {}
                            KeyCode::KeypadBegin => {}
                            KeyCode::Media(_) => {}
                            KeyCode::Modifier(_) => {}
                        }
                    }
                }
                Event::FocusGained => {
                    println!("Event::FocusedGained!!!")
                }
                Event::FocusLost => {
                    println!("Event::FocusedLost!!!")
                }
                Event::Mouse(_) => {
                    println!("Event::Mouse!!!")
                }
                Event::Paste(_) => {
                    println!("Event::Paste!!!")
                }
                Event::Resize(cols, rows) => {
                    // Update the parser with the new terminal size
                    parser.write().unwrap().set_size(rows, cols);
                }
            }
        }
    }
}

fn ui(f: &mut Frame, screen: &Screen) {
    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .margin(1)
        .constraints(
            [
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Percentage(96),
                ratatui::layout::Constraint::Min(1),
            ]
            .as_ref(),
        )
        .split(f.area());

    //header
    let header = "Press \\ to exit".to_string();
    let header = Paragraph::new(header)
        .style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .alignment(Alignment::Center);
    f.render_widget(header, chunks[0]);

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().add_modifier(Modifier::BOLD));
    let pseudo_term = PseudoTerminal::new(screen).block(block);
    f.render_widget(pseudo_term, chunks[1]);

    //footer
    let explanation = "Press \\ to exit".to_string();
    let explanation = Paragraph::new(explanation)
        .style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .alignment(Alignment::Center);
    f.render_widget(explanation, chunks[2]);
}
