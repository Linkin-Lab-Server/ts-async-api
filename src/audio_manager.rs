use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    sync::{mpsc::{UnboundedReceiver, UnboundedSender}, Mutex as TokioMutex},
    task::JoinHandle,
};

use tsclientlib::{ClientId, audio::AudioHandler};

/// The usual amount of samples in a frame.
///
/// Use 48 kHz, 20 ms frames (50 per second) and mono data (1 channel).
/// This means 1920 samples and 7.5 kiB.
const USUAL_FRAME_SIZE: usize = 48000 / 50;

// 最长 30s 切一次音频
const FRAME_SPLIT_COUNT: usize = 1000 * 30 / 20;

static SLEEP_HANDLE: LazyLock<async_spin_sleep::Handle> = LazyLock::new(|| {
    let (sleep_handle, driver) = async_spin_sleep::create();
    let __aenter__ = std::thread::spawn(driver);
    sleep_handle
});

type AudioRx = Arc<TokioMutex<UnboundedReceiver<(ClientId, Vec<u8>)>>>;

pub struct AudioManager {
    audio_handler: JoinHandle<()>,
    pub rx: AudioRx,
}

impl AudioManager {
    pub fn new(audio_handler: Arc<Mutex<AudioHandler<ClientId>>>) -> Self {
        let ctx = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(ClientId, Vec<u8>)>();
        let audio_handler = tokio::spawn(Self::load_audio(ctx.clone(), audio_handler, tx));
        Self { audio_handler, rx: Arc::new(TokioMutex::new(rx)) }
    }

    pub async fn close(self) {
        let Self {
            rx: _,
            audio_handler,
        } = self;
        audio_handler.abort();
        let _ = audio_handler.await;
    }

    async fn load_audio(
        ctx: Arc<Mutex<HashMap<ClientId, Vec<[f32; USUAL_FRAME_SIZE * 2]>>>>,
        audio_handler: Arc<Mutex<AudioHandler<ClientId>>>,
        tx: UnboundedSender<(ClientId, Vec<u8>)>,
    ) {
        let mut prev_talk_id_list: HashMap<ClientId, u16> = HashMap::new();
        loop {
            let mut talk_id_list: HashSet<ClientId> = HashSet::new();
            {
                let mut audio_handler = audio_handler.lock();
                let mut all_buf = [0.0f32; USUAL_FRAME_SIZE * 2];
                audio_handler.fill_buffer_with_proc(&mut all_buf, |client_id, buf| {
                    Self::load_client_audio(
                        ctx.clone(),
                        &mut talk_id_list,
                        client_id,
                        buf,
                    );
                });
                for talk_id in &talk_id_list {
                    *prev_talk_id_list.entry(*talk_id).or_insert(0) = 0;
                }

                for (client_id, client_queue) in ctx.lock().iter_mut() {
                    let mut remove = client_queue.len() >= FRAME_SPLIT_COUNT;

                    if let Some(prev_talk_time) = prev_talk_id_list.get_mut(client_id)
                        && !talk_id_list.contains(client_id)
                    {
                        client_queue.push([0.0f32; USUAL_FRAME_SIZE * 2]);
                        *prev_talk_time += 1;
                        // 大于 500ms
                        if *prev_talk_time > 500 / 20 {
                            remove = true;
                            prev_talk_id_list.remove(client_id);
                        }
                    }

                    if remove {
                        let client_queue = std::mem::take(client_queue);
                        // 讲话完毕, 收集音频
                        let _ = tx
                            .send((*client_id, save_f32_wav(&client_queue)))
                            .inspect_err(|err| {
                                log::error!("send client_queue data to tx fail, err: {err:?}")
                            });
                    }
                }
            }
            SLEEP_HANDLE.sleep_for(Duration::from_millis(20)).await;
        }
    }

    fn load_client_audio(
        ctx: Arc<Mutex<HashMap<ClientId, Vec<[f32; USUAL_FRAME_SIZE * 2]>>>>,
        talk_id_list: &mut HashSet<ClientId>,
        client_id: &ClientId,
        buf: &[f32],
    ) {
        talk_id_list.insert(*client_id);
        let mut ctx = ctx.lock();
        let client_queue = ctx.entry(*client_id).or_default();
        let buf: [f32; USUAL_FRAME_SIZE * 2] = if !buf.is_empty() {
            buf.try_into().expect("slice with incorrect length")
        } else {
            [0.0f32; USUAL_FRAME_SIZE * 2]
        };
        client_queue.push(buf);
    }
}

fn save_f32_wav(frame_list: &[[f32; USUAL_FRAME_SIZE * 2]]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut buf = Cursor::new(Vec::new());

    let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
    for frame in frame_list {
        for s in frame {
            writer.write_sample((*s) * 14.0).unwrap();
        }
    }
    writer.finalize().unwrap();
    buf.into_inner()
}
