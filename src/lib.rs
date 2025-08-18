use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::prelude::*;
use parking_lot::Mutex;
use pyo3::{
    IntoPyObjectExt,
    exceptions::PyException,
    prelude::*,
    types::{PyBytes, PyType},
};
use tokio::{sync::Mutex as TokioMutex, task::JoinHandle};
use tsclientlib::{
    ClientId, Connection, DisconnectOptions, Error as TsError, Identity,
    audio::AudioHandler,
    sync::{SyncConnection, SyncConnectionHandle, SyncStreamItem},
};
use tsproto_packets::packets::AudioData;

use crate::audio_manager::AudioManager;

mod audio_manager;
struct TsBotInternal {
    sync_conn_handle: SyncConnectionHandle,
    event_loop_handle: JoinHandle<()>,
    audio_manager: AudioManager,
}

#[pyclass]
struct TsBot(Arc<TokioMutex<Option<TsBotInternal>>>);

fn to_py_err(err: impl Into<anyhow::Error>) -> PyErr {
    let err = err.into();
    Python::with_gil(|py| {
        // PyErr::from_value(PyString::new(py, &format!("{err:?}")).into_any())
        PyErr::from_value(
            PyException::new_err(format!("{err:?}"))
                .value(py)
                .clone()
                .into_any(),
        )
    })
}

#[pymethods]
impl TsBot {
    #[classmethod]
    pub fn connect<'a>(
        cls: &Bound<'a, PyType>,
        address: String,
        identity: String,
        name: String,
        password: String,
    ) -> PyResult<Bound<'a, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(cls.py(), async move {
            let ret = Self(Arc::new(TokioMutex::new(Option::Some(
                TsBotInternal::connect(&address, &identity, name, password)
                    .await
                    .map_err(to_py_err)?,
            ))));

            Python::with_gil(|py| ret.into_py_any(py))
        })
    }

    pub fn wait_audio<'a>(slf: PyRefMut<'a, Self>) -> PyResult<Bound<'a, PyAny>> {
        let internal = slf.0.clone();

        pyo3_async_runtimes::tokio::future_into_py(slf.py(), async move {
            let rx = internal
                .lock()
                .await
                .as_ref()
                .unwrap()
                .audio_manager
                .rx
                .clone();
            let (client_id, audio) = rx.lock().await.recv().await.unwrap();
            Python::with_gil(|py| (client_id.0, audio).into_py_any(py))
        })
    }

    pub fn __aenter__<'a>(slf: PyRefMut<'a, Self>) -> PyResult<Bound<'a, PyAny>> {
        let internal = slf.0.clone();

        pyo3_async_runtimes::tokio::future_into_py(slf.py(), async move {
            Python::with_gil(|py| Self(internal).into_py_any(py))
        })
    }

    pub fn __aexit__<'a>(
        slf: PyRefMut<'a, Self>,
        exc_type: &crate::Bound<'_, crate::PyAny>,
        exc_value: &crate::Bound<'_, crate::PyAny>,
        traceback: &crate::Bound<'_, crate::PyAny>,
    ) -> PyResult<Bound<'a, PyAny>> {
        let internal = slf.0.clone();
        pyo3_async_runtimes::tokio::future_into_py(slf.py(), async move {
            internal
                .lock()
                .await
                .take()
                .ok_or(anyhow!("ts bot is closed!"))
                .map_err(to_py_err)?
                .close()
                .await
                .map_err(to_py_err)?;
            Ok(Python::with_gil(|py| py.None().into_any()))
        })
    }
}

impl TsBotInternal {
    pub async fn connect(
        address: &str,
        identity: &str,
        name: String,
        password: String,
    ) -> Result<Self> {
        let con_config = Connection::build(address);

        // Optionally set the key of this client, otherwise a new key is generated.
        let id = Identity::new_from_str(identity).map_err(to_py_err)?;

        let con_config = con_config
            .log_commands(false)
            .log_packets(false)
            .log_udp_packets(false)
            .identity(id)
            .name(name)
            .password(password);

        // Connect
        let con: SyncConnection = con_config.connect()?.into();
        let mut handle = con.get_handle();

        let audio_handler = Arc::new(Mutex::new(AudioHandler::<ClientId>::new()));
        // Do event handling in another thread
        let audio_handler_clone = audio_handler.clone();

        let event_loop_handle = tokio::spawn(async move {
            let audio_handler = audio_handler_clone;
            con.for_each(|event| Self::event_handler(audio_handler.clone(), event))
                .await
        });

        handle.wait_until_connected().await?;

        let audio_manager = AudioManager::new(audio_handler);

        let welcome_message = handle
            .with_connection(|con| Result::<_>::Ok(con.get_state()?.server.welcome_message.clone()))
            .await??;

        log::info!("Server welcome message: {welcome_message}");

        // Connect
        let ret = Self {
            sync_conn_handle: handle,
            event_loop_handle,
            audio_manager,
        };

        Ok(ret)
    }

    async fn event_handler(
        audio_handler: Arc<Mutex<AudioHandler<ClientId>>>,
        event: Result<SyncStreamItem, TsError>,
    ) {
        let Ok(event) = event else {
            return;
        };

        match event {
            SyncStreamItem::Audio(in_audio_buf) => {
                let from = ClientId(match in_audio_buf.data().data() {
                    AudioData::S2C { from, .. } => *from,
                    AudioData::S2CWhisper { from, .. } => *from,
                    _ => panic!("Can only handle S2C packets but got a C2S packet"),
                });

                let mut audio_handler = audio_handler.lock();
                let _ = audio_handler
                    .handle_packet(from, in_audio_buf)
                    .inspect_err(|err| {
                        log::error!("audio_handler handle_packet err: {err:?}");
                    });
            }
            SyncStreamItem::MessageEvent(_) => {}
            _ => {}
        }
    }

    async fn close(self) -> Result<()> {
        let Self {
            mut sync_conn_handle,
            event_loop_handle,
            audio_manager,
        } = self;
        audio_manager.close().await;
        sync_conn_handle
            .disconnect(DisconnectOptions::new())
            .await?;
        event_loop_handle.await?;
        Ok(())
    }
}

#[pymodule]
fn ts_async_api(py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    pretty_env_logger::init();
    m.add_class::<TsBot>()?;
    Ok(())
}
