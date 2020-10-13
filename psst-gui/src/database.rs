use crate::{
    data::{Album, AlbumType, Artist, AudioAnalysis, AudioAnalysisSegment, Image, Playlist, Track},
    error::Error,
};
use aspotify::{ItemType, Market, Page, PlaylistItemType, Response};
use druid::im::Vector;
use itertools::Itertools;
use psst_core::{access_token::TokenProvider, session::SessionHandle};
use std::{future::Future, sync::Arc, time::Instant};

#[derive(Clone)]
pub struct Web {
    session: SessionHandle,
    token_provider: Arc<TokenProvider>,
    spotify: Arc<aspotify::Client>,
}

impl Web {
    pub fn new(session: SessionHandle) -> Self {
        // Web API access tokens are requested from the `TokenProvider`, not through the
        // usual Spotify Authorization process, but we still need to give _some_
        // credentials to `aspotify::Client`.
        let dummy_credentials = aspotify::ClientCredentials {
            id: String::new(),
            secret: String::new(),
        };
        let spotify = aspotify::Client::new(dummy_credentials);
        Self {
            session,
            spotify: Arc::new(spotify),
            token_provider: Arc::new(TokenProvider::new()),
        }
    }

    async fn client(&self) -> Result<&aspotify::Client, Error> {
        let access_token = self
            .token_provider
            .get(&self.session)
            .map_err(|err| Error::WebApiError(Box::new(err)))?;
        self.spotify
            .set_current_access_token(aspotify::AccessToken {
                token: access_token.token,
                expires: access_token.expires,
                refresh_token: None,
            })
            .await;
        Ok(self.spotify.as_ref())
    }

    async fn with_paging<'a, PerFn, PerFut, MapFn, T, U>(
        &'a self,
        iter_fn: PerFn,
        map_fn: MapFn,
    ) -> Result<Vector<T>, Error>
    where
        PerFn: Fn(&'a aspotify::Client, usize, usize) -> PerFut,
        PerFut: Future<Output = Result<Response<Page<U>>, aspotify::Error>> + 'a,
        MapFn: Fn(U) -> T,
        T: Clone,
    {
        let mut results = Vector::new();
        let mut limit = 50;
        let mut offset = 0;
        loop {
            let page = iter_fn(self.client().await?, limit, offset).await?.data;

            results.extend(page.items.into_iter().map(&map_fn));

            if page.total > results.len() {
                limit = page.limit;
                offset = page.offset + page.limit;
            } else {
                break;
            }
        }
        Ok(results)
    }

    pub async fn load_album(&self, id: &str) -> Result<Album, Error> {
        let result = self.client().await?.albums().get_album(id, None).await?;
        log::info!("expires in: {:?}", result.expires - Instant::now());
        let result = result.data.into();
        Ok(result)
    }

    pub async fn load_artist(&self, id: &str) -> Result<Artist, Error> {
        let result = self
            .client()
            .await?
            .artists()
            .get_artist(id)
            .await?
            .data
            .into();
        Ok(result)
    }

    pub async fn load_saved_albums(&self) -> Result<Vector<Album>, Error> {
        let result = self
            .with_paging(
                |client, limit, offset| client.library().get_saved_albums(limit, offset, None),
                |saved| saved.album.into(),
            )
            .await?;
        Ok(result)
    }

    pub async fn load_saved_tracks(&self) -> Result<Vector<Arc<Track>>, Error> {
        let result = self
            .with_paging(
                |client, limit, offset| client.library().get_saved_tracks(limit, offset, None),
                |saved| Arc::new(Track::from(saved.track)),
            )
            .await?;
        Ok(result)
    }

    pub async fn load_playlists(&self) -> Result<Vector<Playlist>, Error> {
        let result = self
            .with_paging(
                |client, limit, offset| client.playlists().current_users_playlists(limit, offset),
                |playlist| playlist.into(),
            )
            .await?;
        Ok(result)
    }

    pub async fn load_playlist_tracks(&self, id: &str) -> Result<Vector<Arc<Track>>, Error> {
        let result = self
            .with_paging(
                |client, limit, offset| {
                    client
                        .playlists()
                        .get_playlists_items(id, limit, offset, None)
                },
                |item| match item.item {
                    PlaylistItemType::Track(track) => Arc::new(Track::from(track)),
                    PlaylistItemType::Episode(_) => unimplemented!(),
                },
            )
            .await?;
        Ok(result)
    }

    pub async fn load_artist_albums(&self, id: &str) -> Result<Vector<Album>, Error> {
        let result = self
            .with_paging(
                |client, limit, offset| {
                    client
                        .artists()
                        .get_artist_albums(id, None, limit, offset, None)
                },
                |artists_album| artists_album.into(),
            )
            .await?;
        Ok(result)
    }

    pub async fn load_artist_top_tracks(&self, id: &str) -> Result<Vector<Arc<Track>>, Error> {
        let market = Market::FromToken;
        let result = self
            .client()
            .await?
            .artists()
            .get_artist_top(id, market)
            .await?
            .data
            .into_iter()
            .map(|track| Arc::new(Track::from(track)))
            .collect();
        Ok(result)
    }

    pub async fn load_image(&self, uri: &str) -> Result<image::DynamicImage, Error> {
        let image_bytes = reqwest::get(uri).await?.bytes().await?;
        let result = image::load_from_memory(&image_bytes)?;
        Ok(result)
    }

    pub async fn search(
        &self,
        query: &str,
    ) -> Result<(Vector<Artist>, Vector<Album>, Vector<Arc<Track>>), Error> {
        let results = self
            .client()
            .await?
            .search()
            .search(
                query,
                [ItemType::Artist, ItemType::Album, ItemType::Track]
                    .iter()
                    .copied(),
                false,
                25,
                0,
                None,
            )
            .await?
            .data;
        let artists = results
            .artists
            .map_or_else(Vec::new, |page| page.items)
            .into_iter()
            .map_into()
            .collect();
        let albums = results
            .albums
            .map_or_else(Vec::new, |page| page.items)
            .into_iter()
            .map_into()
            .collect();
        let tracks = results
            .tracks
            .map_or_else(Vec::new, |page| page.items)
            .into_iter()
            .map(|track| Arc::new(Track::from(track)))
            .collect();
        Ok((artists, albums, tracks))
    }

    pub async fn analyze_track(&self, id: &str) -> Result<AudioAnalysis, Error> {
        let result = self
            .client()
            .await?
            .tracks()
            .get_analysis(id)
            .await?
            .data
            .into();
        Ok(result)
    }
}

impl From<aspotify::ArtistSimplified> for Artist {
    fn from(artist: aspotify::ArtistSimplified) -> Self {
        Self {
            id: artist.id.unwrap(),
            name: artist.name.into(),
            images: Vector::new(),
        }
    }
}

impl From<aspotify::Artist> for Artist {
    fn from(artist: aspotify::Artist) -> Self {
        Self {
            id: artist.id,
            name: artist.name.into(),
            images: artist.images.into_iter().map_into().collect(),
        }
    }
}

impl From<aspotify::AlbumSimplified> for Album {
    fn from(album: aspotify::AlbumSimplified) -> Self {
        Self {
            album_type: album.album_type.map(AlbumType::from).unwrap_or_default(),
            artists: album.artists.into_iter().map_into().collect(),
            id: album.id.unwrap(),
            images: album.images.into_iter().map_into().collect(),
            name: album.name.into(),
            release_date: album.release_date,
            release_date_precision: album.release_date_precision,
            genres: Vector::new(),
            tracks: Vector::new(),
        }
    }
}

impl From<aspotify::Album> for Album {
    fn from(album: aspotify::Album) -> Self {
        Self {
            album_type: album.album_type.into(),
            artists: album.artists.into_iter().map_into().collect(),
            id: album.id,
            images: album.images.into_iter().map_into().collect(),
            name: album.name.into(),
            release_date: Some(album.release_date),
            release_date_precision: Some(album.release_date_precision),
            genres: album.genres.into_iter().map_into().collect(),
            tracks: album
                .tracks
                .items
                .into_iter()
                .map(|track| Arc::new(Track::from(track)))
                .collect(),
        }
    }
}

impl From<aspotify::ArtistsAlbum> for Album {
    fn from(album: aspotify::ArtistsAlbum) -> Self {
        Self {
            album_type: album.album_type.into(),
            artists: album.artists.into_iter().map_into().collect(),
            id: album.id,
            images: album.images.into_iter().map_into().collect(),
            name: album.name.into(),
            release_date: Some(album.release_date),
            release_date_precision: Some(album.release_date_precision),
            genres: Vector::new(),
            tracks: Vector::new(),
        }
    }
}

impl From<aspotify::AlbumType> for AlbumType {
    fn from(album: aspotify::AlbumType) -> Self {
        match album {
            aspotify::AlbumType::Album => AlbumType::Album,
            aspotify::AlbumType::Single => AlbumType::Single,
            aspotify::AlbumType::Compilation => AlbumType::Compilation,
        }
    }
}

impl From<aspotify::TrackSimplified> for Track {
    fn from(track: aspotify::TrackSimplified) -> Self {
        Self {
            album: None,
            artists: track.artists.into_iter().map_into().collect(),
            disc_number: track.disc_number,
            duration: track.duration.into(),
            explicit: track.explicit,
            id: track.id,
            is_local: track.is_local,
            is_playable: None,
            name: track.name.into(),
            popularity: None,
            track_number: track.track_number,
        }
    }
}

impl From<aspotify::Track> for Track {
    fn from(track: aspotify::Track) -> Self {
        Self {
            album: Some(track.album.into()),
            artists: track.artists.into_iter().map_into().collect(),
            disc_number: track.disc_number,
            duration: track.duration.into(),
            explicit: track.explicit,
            id: track.id,
            is_local: track.is_local,
            is_playable: track.is_playable,
            name: track.name.into(),
            popularity: Some(track.popularity),
            track_number: track.track_number,
        }
    }
}

impl From<aspotify::PlaylistSimplified> for Playlist {
    fn from(playlist: aspotify::PlaylistSimplified) -> Self {
        Self {
            id: playlist.id,
            images: playlist.images.into_iter().map_into().collect(),
            name: playlist.name,
        }
    }
}

impl From<aspotify::Image> for Image {
    fn from(image: aspotify::Image) -> Self {
        Self {
            url: image.url,
            width: image.width,
            height: image.height,
        }
    }
}

impl From<aspotify::AudioAnalysis> for AudioAnalysis {
    fn from(analysis: aspotify::AudioAnalysis) -> Self {
        Self {
            segments: analysis.segments.into_iter().map_into().collect(),
        }
    }
}

impl From<aspotify::Segment> for AudioAnalysisSegment {
    fn from(segment: aspotify::Segment) -> Self {
        Self {
            start: segment.interval.start.into(),
            duration: segment.interval.duration.into(),
            confidence: segment.interval.confidence,
            loudness_start: segment.loudness_start,
            loudness_max_time: segment.loudness_max_time,
            loudness_max: segment.loudness_max,
            pitches: segment.pitches.into(),
            timbre: segment.timbre.into(),
        }
    }
}

impl From<aspotify::Error> for Error {
    fn from(error: aspotify::Error) -> Self {
        Error::WebApiError(Box::new(error))
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::WebApiError(Box::new(error))
    }
}

impl From<image::ImageError> for Error {
    fn from(error: image::ImageError) -> Self {
        Error::WebApiError(Box::new(error))
    }
}
