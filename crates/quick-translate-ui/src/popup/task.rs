//! Helpers that turn effects into iced tasks.

use std::sync::Arc;

use futures::{Stream, StreamExt, stream};
use iced::Task;
use quick_translate_backends::{ChunkStream, Request, Translator};
use tokio_util::sync::CancellationToken;

use super::{Message, Pane, TranslationId};

/// Focuses a pane's text editor, or unfocuses every editor when `None` (focusing an id that
/// no widget has unfocuses all focusable widgets).
pub fn focus_task<T: Send + 'static>(pane: Option<Pane>) -> Task<T> {
    let id = pane.map_or_else(
        || iced::widget::Id::new("quick-translate-no-focus"),
        Pane::widget_id,
    );
    iced::widget::operation::focus(id)
}

/// Runs `request` on `translator` and reports the outcome as popup messages: `Chunk`s, then
/// `TranslationFinished`, or `TranslationFailed` on the first error. Cancelling `cancel`
/// aborts the request.
pub fn translation_stream(
    translator: Arc<dyn Translator>,
    id: TranslationId,
    request: Request,
    cancel: CancellationToken,
) -> impl Stream<Item = Message> + Send + 'static {
    enum State {
        Start(Arc<dyn Translator>, Request, CancellationToken),
        Streaming(ChunkStream),
        Done,
    }

    async fn next(mut chunks: ChunkStream, id: TranslationId) -> Option<(Message, State)> {
        Some(match chunks.next().await {
            Some(Ok(chunk)) => (Message::Chunk { id, chunk }, State::Streaming(chunks)),
            Some(Err(error)) => (Message::TranslationFailed { id, error }, State::Done),
            None => (Message::TranslationFinished { id }, State::Done),
        })
    }

    stream::unfold(
        State::Start(translator, request, cancel),
        move |state| async move {
            match state {
                State::Start(translator, request, cancel) => {
                    match translator.translate(request, cancel).await {
                        Ok(chunks) => next(chunks, id).await,
                        Err(error) => Some((Message::TranslationFailed { id, error }, State::Done)),
                    }
                }
                State::Streaming(chunks) => next(chunks, id).await,
                State::Done => None,
            }
        },
    )
}

/// [`translation_stream`] as an iced task.
pub fn translation_task(
    translator: Arc<dyn Translator>,
    id: TranslationId,
    request: Request,
    cancel: CancellationToken,
) -> Task<Message> {
    Task::stream(translation_stream(translator, id, request, cancel))
}
